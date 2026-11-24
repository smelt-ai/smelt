//! NSH / GPUI 独立性能基准套件。开 1200×800 窗口，按场景预热后采集逐帧/分阶段耗时、
//! 主线程与进程 CPU、内存分布与增长量、线程数及运行时错误。
//!
//! ```text
//! cargo run -j 2 --release -p smelt-nsh --example bench -- --list
//! cargo run -j 2 --release -p smelt-nsh --example bench -- <场景> [--warmup 3] [--measure 6] [--json]
//! ```

mod fixtures;
mod metrics;
mod terminal;

use std::{
    fs,
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use gpui::{
    AnyElement, AnyWindowHandle, App, AppContext as _, Bounds, Context, Entity, IntoElement,
    Modifiers, ParentElement as _, PlatformInput, Render, ScrollDelta, ScrollEvent, ScrollSource,
    SharedString, Styled as _, Subscription, TouchPhase, Window, WindowBounds, WindowId,
    WindowOptions, div, point, px, rgb, size,
};
use nsh_platform_gpui::{
    App as NshApp, AppOptions, AppView, AssetBase, Bundle, ComponentContext, NativeComponent,
};
use serde_json::{Value, json};
use smelt_nsh::{MainShell, MainShellEvent, OutletTarget};
use smelt_plugin_api::{
    self as api, ActiveContext, HostEvent, Project, RuntimeKey, process_usage::ProcessSampler,
};
use smelt_terminal::TerminalCanvas;
use tokio::sync::oneshot;

use crate::{
    fixtures::{
        BootCompileResult, RealConvMode, RealShellSetup, conv_stream_chunks,
        make_conv_assistant_entry, make_conv_tool_entry, run_boot_compile_probe, setup_real_conv,
        setup_real_shell, sync_shell_visibility,
    },
    metrics::{
        BenchReport, BootReport, CpuSample, ExtraEvents, FrameCollector, MemoryReport,
        MemorySnapshot, disable_frame_tracing, enable_frame_tracing, thread_count,
    },
    terminal::{
        ArcTerminalConnector, BenchTerminalConnector, BenchTerminalMode, spawn_real_terminal,
    },
};

const DEFAULT_WARMUP_SECS: f64 = 3.0;
const DEFAULT_MEASURE_SECS: f64 = 6.0;

struct ScenarioInfo {
    name: &'static str,
    summary: &'static str,
}

const SCENARIOS: &[ScenarioInfo] = &[
    ScenarioInfo {
        name: "idle",
        summary: "合成：左 NSH 静态列表 + 右 GPUI 假终端",
    },
    ScenarioInfo {
        name: "split",
        summary: "合成：左 NSH 旋转列表 + 右 GPUI 假终端（多视图）",
    },
    ScenarioInfo {
        name: "whole",
        summary: "合成：整窗 NSH + 原生假终端组件（每帧新建元素）",
    },
    ScenarioInfo {
        name: "split4",
        summary: "合成：左 NSH 旋转列表 + 右 2×2 GPUI 假终端",
    },
    ScenarioInfo {
        name: "whole4",
        summary: "合成：整窗 NSH + 2×2 原生假终端组件",
    },
    ScenarioInfo {
        name: "wholev",
        summary: "合成：整窗 NSH + View 缓存假终端",
    },
    ScenarioInfo {
        name: "whole4v",
        summary: "合成：整窗 NSH + 2×2 View 缓存假终端",
    },
    ScenarioInfo {
        name: "gpui",
        summary: "合成：纯 GPUI 对照（左旋转列表 + 右假终端）",
    },
    ScenarioInfo {
        name: "views",
        summary: "合成：N 块空闲 NSH 列表网格（第二参数传块数）",
    },
    ScenarioInfo {
        name: "chat-stream-idle",
        summary: "合成：200 条对话流探针（空闲）",
    },
    ScenarioInfo {
        name: "chat-stream-30",
        summary: "合成：200 条对话流探针（30Hz 吐字）",
    },
    ScenarioInfo {
        name: "chat-stream",
        summary: "合成：200 条对话流探针（60Hz 吐字）",
    },
    ScenarioInfo {
        name: "real-conv-idle",
        summary: "真实：<SmeltConversation> 200 条混合消息（含 Mermaid）空闲态",
    },
    ScenarioInfo {
        name: "real-conv-sweep",
        summary: "真实：<SmeltConversation> 200 条消息 + Composer 扫光动效",
    },
    ScenarioInfo {
        name: "real-conv-stream-30",
        summary: "真实：<SmeltConversation> 200 条消息 + 扫光 + 30Hz 吐字",
    },
    ScenarioInfo {
        name: "real-conv-stream-60",
        summary: "真实：<SmeltConversation> 200 条消息 + 扫光 + 60Hz 吐字",
    },
    ScenarioInfo {
        name: "real-conv-grow",
        summary: "真实：<SmeltConversation> 流式吐字 + 持续追加 ToolCall/Markdown/Mermaid 条目",
    },
    ScenarioInfo {
        name: "real-conv-scroll",
        summary: "真实：<SmeltConversation> 200 条消息 60Hz 滚轮往返滚动",
    },
    ScenarioInfo {
        name: "real-term-idle",
        summary: "真实：单个 TerminalCanvas（200 行彩色输出后静默）",
    },
    ScenarioInfo {
        name: "real-term-flood",
        summary: "真实：单个 TerminalCanvas 高频 ANSI 256 色刷屏",
    },
    ScenarioInfo {
        name: "real-term-4",
        summary: "真实：2×2 共 4 个 TerminalCanvas 并发高频刷屏",
    },
    ScenarioInfo {
        name: "real-term-and-conv",
        summary: "真实：左 <SmeltConversation> 30Hz 流式 + 右 TerminalCanvas 高频刷屏",
    },
    ScenarioInfo {
        name: "real-boot",
        summary: "真实：MainShell + 全量源码插件编译耗时与挂载后空闲代价",
    },
    ScenarioInfo {
        name: "real-boot-staged",
        summary: "真实：MainShell + 预编译字节码插件冷启动与空闲代价",
    },
    ScenarioInfo {
        name: "real-state-storm",
        summary: "真实：MainShell + 20Hz SessionsChanged 广播风暴",
    },
    ScenarioInfo {
        name: "real-git-diff",
        summary: "真实：MainShell + Git 40 文件大 Diff 视图 60Hz 滚动",
    },
    ScenarioInfo {
        name: "real-files-editor",
        summary: "真实：MainShell + 文件树与 800 行 Rust 源码编辑器 60Hz 滚动",
    },
    ScenarioInfo {
        name: "real-session-switch",
        summary: "真实：MainShell + 10 个工作区会话每 250ms 往返切换",
    },
    ScenarioInfo {
        name: "real-resize",
        summary: "真实：MainShell + 15Hz 窗口连续缩放（含布局与重绘）",
    },
];

struct CliArgs {
    mode: String,
    count: usize,
    warmup: Duration,
    measure: Duration,
    json: bool,
    list: bool,
}

fn parse_cli() -> Result<CliArgs, String> {
    let mut args = std::env::args().skip(1);
    let mut positionals: Vec<String> = Vec::new();
    let mut warmup_s = DEFAULT_WARMUP_SECS;
    let mut measure_s = DEFAULT_MEASURE_SECS;
    let mut json = false;
    let mut list = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--list" => list = true,
            "--json" => json = true,
            "--warmup" => {
                let raw = args.next().ok_or("--warmup 需要秒数参数")?;
                warmup_s = raw
                    .parse::<f64>()
                    .map_err(|_| format!("--warmup 非法数字：{raw}"))?;
            }
            "--measure" => {
                let raw = args.next().ok_or("--measure 需要秒数参数")?;
                measure_s = raw
                    .parse::<f64>()
                    .map_err(|_| format!("--measure 非法数字：{raw}"))?;
            }
            "--help" | "-h" => {
                list = true;
            }
            other if other.starts_with("--") => {
                return Err(format!("不认识的选项：{other}（用 --list 看所有场景）"));
            }
            _ => positionals.push(arg),
        }
    }

    if warmup_s < 0.0 || measure_s <= 0.0 {
        return Err("--warmup 须 >= 0，--measure 须 > 0".to_owned());
    }

    let mode = positionals
        .first()
        .cloned()
        .unwrap_or_else(|| "split".to_owned());
    let count = positionals
        .get(1)
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(1);

    Ok(CliArgs {
        mode,
        count,
        warmup: Duration::from_secs_f64(warmup_s),
        measure: Duration::from_secs_f64(measure_s),
        json,
        list,
    })
}

fn print_scenarios() {
    println!(
        "可用性能基准场景（用法：bench <场景> [块数] [--warmup 秒] [--measure 秒] [--json]）："
    );
    for item in SCENARIOS {
        println!("  {:<22} {}", item.name, item.summary);
    }
}

fn main() {
    let cli = match parse_cli() {
        Ok(cli) => cli,
        Err(err) => {
            eprintln!("参数错误：{err}");
            std::process::exit(2);
        }
    };
    if cli.list {
        print_scenarios();
        return;
    }

    if !SCENARIOS.iter().any(|s| s.name == cli.mode) {
        eprintln!("不认识的场景 `{}`（可用 --list 列出所有场景）", cli.mode);
        std::process::exit(2);
    }

    let logger = Box::leak(Box::new(Logger {
        quiet_fps: cli.json,
    }));
    log::set_logger(logger).expect("install bench logger");
    log::set_max_level(if cli.json {
        log::LevelFilter::Warn
    } else {
        log::LevelFilter::Info
    });

    let boot_compile = if cli.mode == "real-boot" {
        Some(run_boot_compile_probe())
    } else {
        None
    };

    enable_frame_tracing();

    gpui_platform::application().run(move |cx| {
        nsh_platform_gpui::set_ui_sets(vec![
            nsh_base::registry(),
            smelt_nsh::testing::host_registry(),
        ]);
        let bounds = Bounds::centered(None, size(px(1200.), px(800.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            inactive_frame_interval: None,
            ..WindowOptions::default()
        };
        let cli_shared = Rc::new(cli);
        let boot_shared = Rc::new(boot_compile);
        cx.open_window(options, move |window, cx| {
            let cli = Rc::clone(&cli_shared);
            let boot = Rc::clone(&boot_shared);
            cx.new(|cx| Bench::new(&cli, boot.as_ref().as_ref(), window, cx))
        })
        .expect("open bench window");
        cx.activate(true);
    });
}

struct Logger {
    quiet_fps: bool,
}

impl log::Log for Logger {
    fn enabled(&self, meta: &log::Metadata) -> bool {
        if meta.level() <= log::Level::Warn {
            return true;
        }
        !self.quiet_fps && meta.target().starts_with("nsh::fps")
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!("[{}] {}", record.target(), record.args());
        }
    }

    fn flush(&self) {}
}

enum Layout {
    Split(gpui::AnyView, Vec<Entity<GPuiFakeTerminal>>),
    Whole(Entity<AppView>),
    Shell(Entity<MainShell>),
    Grid(Vec<Entity<AppView>>),
    RealTerminals(Vec<Entity<TerminalCanvas>>),
    TermAndConv(Entity<AppView>, Entity<TerminalCanvas>),
}

struct Bench {
    layout: Layout,
    _subscriptions: Vec<Subscription>,
}

impl Bench {
    fn new(
        cli: &CliArgs,
        boot_compile: Option<&BootCompileResult>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mode = cli.mode.as_str();
        let count = cli.count;
        let window_handle = window.window_handle();
        let window_id: WindowId = window_handle.window_id();

        let mut subscriptions = Vec::new();
        let mut temp_dirs: Vec<PathBuf> = Vec::new();
        let mut conv_errors: Option<Arc<AtomicUsize>> = None;
        let mut shell_errors: Option<Arc<AtomicUsize>> = None;
        let mut ready_wait: Option<oneshot::Receiver<()>> = None;
        let mut shell_setup: Option<RealShellSetup> = None;
        let mut conv_stream = None;
        let mut term_controller: Option<Arc<BenchTerminalConnector>> = None;
        let mut expected_term_connections = 0usize;
        let mut boot_t0: Option<Instant> = None;

        let layout = match mode {
            "idle" => Layout::Split(
                nsh(window, cx, "bench-side-idle").into(),
                vec![cx.new(|_| GPuiFakeTerminal)],
            ),
            "split" => Layout::Split(
                nsh(window, cx, "bench-side").into(),
                vec![cx.new(|_| GPuiFakeTerminal)],
            ),
            "whole" => Layout::Whole(nsh(window, cx, "bench-whole")),
            "split4" => Layout::Split(
                nsh(window, cx, "bench-side").into(),
                (0..4).map(|_| cx.new(|_| GPuiFakeTerminal)).collect(),
            ),
            "whole4" => Layout::Whole(nsh(window, cx, "bench-whole4")),
            "wholev" | "whole4v" => {
                VIEW_BACKED.store(true, Ordering::Relaxed);
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
                vec![cx.new(|_| GPuiFakeTerminal)],
            ),
            "views" => Layout::Grid(
                (0..count)
                    .map(|_| nsh(window, cx, "bench-side-idle"))
                    .collect(),
            ),
            "chat-stream" => Layout::Whole(nsh(window, cx, "bench-chat-stream")),
            "chat-stream-30" => Layout::Whole(nsh(window, cx, "bench-chat-stream-30")),
            "chat-stream-idle" => Layout::Whole(nsh(window, cx, "bench-chat-stream-idle")),
            "real-conv-idle"
            | "real-conv-sweep"
            | "real-conv-stream-30"
            | "real-conv-stream-60"
            | "real-conv-grow"
            | "real-conv-scroll" => {
                let kind = match mode {
                    "real-conv-idle" => RealConvMode::Idle,
                    "real-conv-sweep" => RealConvMode::Sweep,
                    "real-conv-stream-30" => RealConvMode::Stream30,
                    "real-conv-stream-60" => RealConvMode::Stream60,
                    "real-conv-grow" => RealConvMode::Grow,
                    "real-conv-scroll" => RealConvMode::Scroll,
                    _ => unreachable!(),
                };
                let setup = setup_real_conv(kind, window, cx);
                temp_dirs.push(setup.stage);
                conv_errors = Some(setup.errors);
                ready_wait = Some(setup.ready_rx);
                conv_stream = setup.stream;
                Layout::Whole(setup.app)
            }
            "real-term-idle" | "real-term-flood" | "real-term-4" => {
                let term_mode = if mode == "real-term-idle" {
                    BenchTerminalMode::IdlePopulated
                } else {
                    BenchTerminalMode::Flood
                };
                let n = if mode == "real-term-4" { 4 } else { 1 };
                let ctrl = BenchTerminalConnector::new(term_mode);
                let arc_conn = ArcTerminalConnector::new(Arc::clone(&ctrl));
                install_minimal_terminal_host(Arc::clone(&arc_conn), cx);
                let canvases = (0..n)
                    .map(|i| spawn_real_terminal(&format!("bench-term-{i}"), window, cx))
                    .collect();
                term_controller = Some(ctrl);
                expected_term_connections = n;
                Layout::RealTerminals(canvases)
            }
            "real-term-and-conv" => {
                let ctrl = BenchTerminalConnector::new(BenchTerminalMode::Flood);
                let arc_conn = ArcTerminalConnector::new(Arc::clone(&ctrl));
                install_minimal_terminal_host(Arc::clone(&arc_conn), cx);
                let conv = setup_real_conv(RealConvMode::Stream30, window, cx);
                temp_dirs.push(conv.stage);
                conv_errors = Some(conv.errors);
                ready_wait = Some(conv.ready_rx);
                conv_stream = conv.stream;
                let term = spawn_real_terminal("bench-term-split", window, cx);
                term_controller = Some(ctrl);
                expected_term_connections = 1;
                Layout::TermAndConv(conv.app, term)
            }
            "real-boot"
            | "real-boot-staged"
            | "real-state-storm"
            | "real-git-diff"
            | "real-files-editor"
            | "real-session-switch"
            | "real-resize" => {
                let staged = matches!(
                    mode,
                    "real-boot-staged"
                        | "real-git-diff"
                        | "real-files-editor"
                        | "real-session-switch"
                        | "real-resize"
                );
                let connector = if mode == "real-session-switch" {
                    let ctrl = BenchTerminalConnector::new(BenchTerminalMode::IdlePopulated);
                    let arc_conn = ArcTerminalConnector::new(Arc::clone(&ctrl));
                    term_controller = Some(ctrl);
                    Some(arc_conn)
                } else {
                    None
                };
                let setup = setup_real_shell(staged, connector, window, cx);
                boot_t0 = Some(setup.boot_started);
                if let Some(dir) = setup.temp_dir.clone() {
                    temp_dirs.push(dir);
                }
                let shell = setup.shell.clone();
                let err_counter = Arc::new(AtomicUsize::new(0));
                shell_errors = Some(Arc::clone(&err_counter));
                shell.update(cx, |s, cx| {
                    s.app_view().update(cx, |view, _| {
                        view.set_fps_monitor(false);
                        view.on_app_error(move |error| {
                            err_counter.fetch_add(1, Ordering::Relaxed);
                            eprintln!("[bench-shell-error] {error}");
                        });
                    });
                });

                let (boot_tx, boot_rx) = oneshot::channel::<()>();
                let boot_tx = Arc::new(Mutex::new(Some(boot_tx)));
                if shell.read(cx).initial_plugins_settled()
                    && let Some(tx) = boot_tx.lock().expect("boot_tx poisoned").take()
                {
                    let _ = tx.send(());
                }
                let sub = cx.subscribe(&shell, move |_, _, ev: &MainShellEvent, _| {
                    if matches!(ev, MainShellEvent::InitialReady)
                        && let Some(tx) = boot_tx.lock().expect("boot_tx poisoned").take()
                    {
                        let _ = tx.send(());
                    }
                });
                subscriptions.push(sub);
                ready_wait = Some(boot_rx);
                shell_setup = Some(setup);
                Layout::Shell(shell)
            }
            other => panic!("不认识的场景 {other}"),
        };

        let mode_owned = mode.to_owned();
        let warmup_dur = cli.warmup;
        let measure_dur = cli.measure;
        let emit_json = cli.json;
        let boot_compile_owned = boot_compile.map(|b| BootCompileResult {
            compile_total_ms: b.compile_total_ms,
            compile_shell_ms: b.compile_shell_ms,
            compile_plugins_ms: b.compile_plugins_ms,
            slowest_plugins: b.slowest_plugins.clone(),
        });

        cx.spawn(async move |_, cx| {
            // 1. 等异步初始化完成（插件安装、ACP 订阅首包、终端握手）
            if let Some(rx) = ready_wait {
                wait_oneshot(rx, &mode_owned, cx).await;
            }
            if expected_term_connections > 0
                && let Some(ctrl) = term_controller.as_ref()
            {
                let start = Instant::now();
                while ctrl.connected_count() < expected_term_connections {
                    assert!(
                        start.elapsed() < Duration::from_secs(15),
                        "等待 {mode_owned} 终端握手超时"
                    );
                    cx.background_executor()
                        .timer(Duration::from_millis(10))
                        .await;
                }
            }

            // 2. 全量主壳装载出厂布局，并准备对应场景初始视图/上下文
            let mut boot_report: Option<BootReport> = None;
            let mut setup_keepalive = None;
            if let Some(setup) = shell_setup {
                let wait_layout_start = Instant::now();
                let standard_state = loop {
                    if let Some(st) = cx.update(|cx| api::layout("smelt.layouts/standard", cx)) {
                        break st;
                    }
                    assert!(
                        wait_layout_start.elapsed() < Duration::from_secs(10),
                        "等待 smelt.layouts/standard 超时"
                    );
                    cx.background_executor()
                        .timer(Duration::from_millis(10))
                        .await;
                };

                let host_state = setup.host_state.clone();
                let shell = setup.shell.clone();
                let (plugin_count, plugin_vms, plugin_errs) = shell.update(cx, |s, cx| {
                    s.load_page("smelt.layouts/workbench".into(), &standard_state, cx)
                        .expect("装入 standard.json 布局");
                    s.set_active_page(
                        "smelt.layouts/workbench".into(),
                        Some(OutletTarget {
                            id: "smelt.sessions/session".into(),
                            args: Value::Null,
                            seq: 1,
                            managed_rust_instance: false,
                        }),
                        cx,
                    );
                    sync_shell_visibility(s.shell_view().clone(), &host_state, cx);
                    let list = s.installed_plugins();
                    let errs = list.iter().filter(|p| p.error.is_some()).count();
                    (list.len(), s.plugin_vm_ids().len(), errs)
                });
                if let Some(counter) = shell_errors.as_ref() {
                    counter.fetch_add(plugin_errs, Ordering::Relaxed);
                }

                if let Some(t0) = boot_t0
                    && matches!(mode_owned.as_str(), "real-boot" | "real-boot-staged")
                {
                    let ready_ms = t0.elapsed().as_secs_f64() * 1000.0;
                    let runtime_errs = shell_errors
                        .as_ref()
                        .map_or(0, |c| c.load(Ordering::Relaxed));
                    eprintln!(
                        "BOOT_READY: {ready_ms:.1}ms (plugins={plugin_count}, plugin_vms={plugin_vms}, plugin_errors={plugin_errs}, runtime_errors={runtime_errs})"
                    );
                    boot_report = Some(BootReport {
                        compile_total_ms: boot_compile_owned
                            .as_ref()
                            .map(|b| b.compile_total_ms),
                        compile_shell_ms: boot_compile_owned
                            .as_ref()
                            .map(|b| b.compile_shell_ms),
                        compile_plugins_ms: boot_compile_owned
                            .as_ref()
                            .map(|b| b.compile_plugins_ms),
                        stage_ms: setup.stage_ms,
                        ready_ms,
                        plugins: plugin_count,
                        plugin_vms,
                        plugin_errors: plugin_errs + runtime_errs,
                        slowest_plugins: boot_compile_owned
                            .as_ref()
                            .map_or_else(Vec::new, |b| b.slowest_plugins.clone()),
                    });
                }

                // 设置场景需要的活动项目或前置视图
                if matches!(
                    mode_owned.as_str(),
                    "real-git-diff" | "real-files-editor" | "real-session-switch" | "real-resize"
                ) {
                    cx.update(|cx| {
                        api::set_active_context(
                            ActiveContext {
                                project: Some(Project {
                                    machine: RuntimeKey::Local,
                                    root: "/work/smelt".into(),
                                }),
                                session: None,
                            },
                            cx,
                        );
                    });
                }

                if mode_owned == "real-git-diff" {
                    let wait_view = Instant::now();
                    loop {
                        let ready = cx.update(|cx| api::is_registered("smelt.git/main", cx));
                        if ready {
                            break;
                        }
                        assert!(
                            wait_view.elapsed() < Duration::from_secs(10),
                            "等待 smelt.git/main 注册超时"
                        );
                        cx.background_executor()
                            .timer(Duration::from_millis(10))
                            .await;
                    }
                    shell.update(cx, |s, cx| {
                        s.show_view("smelt.layouts/workbench", "smelt.git/main", cx);
                        sync_shell_visibility(s.shell_view().clone(), &host_state, cx);
                    });
                    let wait_rpc = Instant::now();
                    while !host_state
                        .rpc_counters
                        .git_diff_all_served
                        .load(Ordering::Acquire)
                    {
                        assert!(
                            wait_rpc.elapsed() < Duration::from_secs(15),
                            "等待 git.diff.all 返回超时"
                        );
                        cx.background_executor()
                            .timer(Duration::from_millis(10))
                            .await;
                    }
                } else if mode_owned == "real-files-editor" {
                    let wait_cmd = Instant::now();
                    loop {
                        let ready = cx.update(|cx| {
                            api::command("smelt.files/open", cx).is_some()
                                && api::is_registered("smelt.files/main", cx)
                        });
                        if ready {
                            break;
                        }
                        assert!(
                            wait_cmd.elapsed() < Duration::from_secs(10),
                            "等待 smelt.files/open 注册超时"
                        );
                        cx.background_executor()
                            .timer(Duration::from_millis(10))
                            .await;
                    }
                    let open_task = cx.update(|cx| {
                        api::run(
                            "smelt.files/open",
                            json!({
                                "machine": null,
                                "path": "/work/smelt/src/large_module.rs"
                            }),
                            cx,
                        )
                    });
                    let _ = open_task.await;
                    let wait_rpc = Instant::now();
                    while !host_state
                        .rpc_counters
                        .file_open_served
                        .load(Ordering::Acquire)
                    {
                        assert!(
                            wait_rpc.elapsed() < Duration::from_secs(15),
                            "等待 file.open 返回超时"
                        );
                        cx.background_executor()
                            .timer(Duration::from_millis(10))
                            .await;
                    }
                } else if mode_owned == "real-session-switch" {
                    let wait_cmd = Instant::now();
                    loop {
                        let ready =
                            cx.update(|cx| api::command("smelt.sessions/activate", cx).is_some());
                        if ready {
                            break;
                        }
                        assert!(
                            wait_cmd.elapsed() < Duration::from_secs(10),
                            "等待 smelt.sessions/activate 注册超时"
                        );
                        cx.background_executor()
                            .timer(Duration::from_millis(10))
                            .await;
                    }
                    let task = cx.update(|cx| {
                        api::run(
                            "smelt.sessions/activate",
                            json!({ "machine": null, "session_id": "t-0" }),
                            cx,
                        )
                    });
                    let _ = task.await;
                }

                setup_keepalive = Some(setup);
            }

            // 3. 若是真实对话流式场景，启动后台推送任务
            let stream_stop = Arc::new(AtomicBool::new(false));
            let appended_counter = Arc::new(AtomicUsize::new(0));
            if let Some(driver) = conv_stream {
                let stop = Arc::clone(&stream_stop);
                let appended = Arc::clone(&appended_counter);
                cx.spawn(async move |cx| {
                    let chunks = conv_stream_chunks();
                    let sub_id = driver
                        .sub_id
                        .lock()
                        .expect("sub_id lock poisoned")
                        .clone()
                        .expect("acp.subscribe 应已写入 sub_id");
                    let mut chunk_ix = 0usize;
                    let mut rev = 2u64;
                    let mut acc = String::new();
                    let mut total_entries = driver.initial_count;
                    let mut last_append = Instant::now();

                    while !stop.load(Ordering::Relaxed) {
                        cx.background_executor().timer(driver.interval).await;
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        if chunk_ix >= chunks.len() {
                            chunk_ix = 0;
                            acc.clear();
                        } else {
                            acc.push_str(&chunks[chunk_ix]);
                            chunk_ix += 1;
                        }

                        if driver.grows && last_append.elapsed() >= Duration::from_millis(150) {
                            last_append = Instant::now();
                            let seq_n = appended.fetch_add(1, Ordering::Relaxed);
                            let tail_idx = total_entries.saturating_sub(1);
                            let new_tool = make_conv_tool_entry(tail_idx);
                            let new_assistant =
                                make_conv_assistant_entry(tail_idx + 1, seq_n.is_multiple_of(4));
                            total_entries += 2;
                            rev += 1;
                            driver.host.send_to(
                                "probe",
                                "smelt.acp.event",
                                json!({
                                    "sub_id": sub_id,
                                    "kind": "snapshot",
                                    "snapshot": {
                                        "phase": "thinking",
                                        "replaying_history": false,
                                        "envelope_omitted": true,
                                        "entries_offset": tail_idx,
                                        "entries_total": total_entries,
                                        "snapshot_revision": rev,
                                        "entries": [
                                            new_tool,
                                            new_assistant,
                                            { "Assistant": { "text": acc, "thought": false } },
                                        ],
                                    },
                                }),
                            );
                        } else {
                            rev += 1;
                            driver.host.send_to(
                                "probe",
                                "smelt.acp.event",
                                json!({
                                    "sub_id": sub_id,
                                    "kind": "snapshot",
                                    "snapshot": {
                                        "phase": "thinking",
                                        "replaying_history": false,
                                        "envelope_omitted": true,
                                        "entries_offset": total_entries - 1,
                                        "entries_total": total_entries,
                                        "snapshot_revision": rev,
                                        "entries": [{
                                            "Assistant": { "text": acc, "thought": false }
                                        }],
                                    },
                                }),
                            );
                        }
                    }
                })
                .detach();
            }

            // 4. 预热窗口
            cx.background_executor().timer(warmup_dur).await;

            // 5. 测量窗口起点：清零帧采集器、记录 CPU / 内存 / 假终端重绘基线
            let mut frame_collector = FrameCollector::new(window_id);
            frame_collector.reset();
            let mut mem_sampler = ProcessSampler::new();
            let mem_before = MemorySnapshot::capture(&mut mem_sampler);
            let cpu_before = CpuSample::now();
            let term_before = TERMINAL_RENDERS.load(Ordering::Relaxed);
            appended_counter.store(0, Ordering::Relaxed);

            let measure_t0 = Instant::now();
            let mut extra = ExtraEvents::default();

            match mode_owned.as_str() {
                "real-state-storm" => {
                    let rev_counter = setup_keepalive
                        .as_ref()
                        .expect("shell setup")
                        .host_state
                        .dir_revision
                        .clone();
                    let mut broadcasts = 0usize;
                    while measure_t0.elapsed() < measure_dur {
                        cx.background_executor()
                            .timer(Duration::from_millis(50))
                            .await;
                        rev_counter.fetch_add(1, Ordering::Relaxed);
                        broadcasts += 1;
                        cx.update(|cx| api::emit(HostEvent::SessionsChanged, cx));
                    }
                    extra.broadcasts = Some(broadcasts);
                }
                "real-conv-scroll" | "real-git-diff" | "real-files-editor" => {
                    let (pos, first_dir) = if mode_owned == "real-conv-scroll" {
                        // 对话默认粘在底部：先向上滚（+dy），再向下滚（-dy）。
                        (point(px(600.), px(400.)), 1.0_f32)
                    } else {
                        // Diff / 文件编辑器默认在顶部：先向下滚（-dy），再向上滚（+dy）。
                        (point(px(930.), px(420.)), -1.0_f32)
                    };
                    let mut scrolls = 0usize;
                    while measure_t0.elapsed() < measure_dur {
                        cx.background_executor()
                            .timer(Duration::from_millis(16))
                            .await;
                        let phase_ms = measure_t0.elapsed().as_millis() % 2000;
                        let dy = if phase_ms < 1000 {
                            48.0 * first_dir
                        } else {
                            -48.0 * first_dir
                        };
                        let _ = cx.update_window(window_handle, |_, window, cx| {
                            window.dispatch_event(
                                PlatformInput::Scroll(ScrollEvent {
                                    position: pos,
                                    delta: ScrollDelta::Pixels(point(px(0.), px(dy))),
                                    modifiers: Modifiers::default(),
                                    touch_phase: TouchPhase::Moved,
                                    source: ScrollSource::default(),
                                }),
                                cx,
                            );
                        });
                        scrolls += 1;
                    }
                    extra.scroll_events = Some(scrolls);
                }
                "real-session-switch" => {
                    let mut switches = 0usize;
                    while measure_t0.elapsed() < measure_dur {
                        cx.background_executor()
                            .timer(Duration::from_millis(250))
                            .await;
                        let target = format!("t-{}", (switches + 1) % 10);
                        let task = cx.update(|cx| {
                            api::run(
                                "smelt.sessions/activate",
                                json!({ "machine": null, "session_id": target }),
                                cx,
                            )
                        });
                        let _ = task.await;
                        switches += 1;
                    }
                    extra.switches = Some(switches);
                }
                "real-resize" => {
                    const SIZES: [(f32, f32); 3] = [(1200., 800.), (1440., 900.), (960., 680.)];
                    let mut resizes = 0usize;
                    while measure_t0.elapsed() < measure_dur {
                        cx.background_executor()
                            .timer(Duration::from_millis(66))
                            .await;
                        let (w, h) = SIZES[resizes % SIZES.len()];
                        let _ = cx.update_window(window_handle, |_, window, _| {
                            window.resize(size(px(w), px(h)));
                        });
                        resizes += 1;
                    }
                    extra.resizes = Some(resizes);
                }
                _ => {
                    cx.background_executor().timer(measure_dur).await;
                }
            }

            if mode_owned == "real-conv-grow" {
                extra.appended_entries = Some(appended_counter.load(Ordering::Relaxed));
            }

            // 6. 测量终点采样
            let wall_s = measure_t0.elapsed().as_secs_f64().max(0.001);
            let cpu_after = CpuSample::now();
            let mem_after = MemorySnapshot::capture(&mut mem_sampler);
            let term_after = TERMINAL_RENDERS.load(Ordering::Relaxed);
            let frame_report = frame_collector.finish(wall_s);
            let cpu_report = cpu_before.diff(cpu_after, wall_s, frame_report.drawn_frames);
            let memory_report = MemoryReport::from_pair(mem_before, mem_after);
            let threads = thread_count();

            stream_stop.store(true, Ordering::Relaxed);
            if let Some(ctrl) = term_controller {
                ctrl.stop();
            }

            let errors = conv_errors
                .as_ref()
                .or(shell_errors.as_ref())
                .map_or(0, |c| c.load(Ordering::Relaxed));
            let terminal_renders_per_frame = if frame_report.drawn_frames > 0 {
                (term_after - term_before) as f64 / frame_report.drawn_frames as f64
            } else {
                0.0
            };

            let report = BenchReport {
                scenario: mode_owned,
                count,
                warmup_s: warmup_dur.as_secs_f64(),
                measure_s: measure_dur.as_secs_f64(),
                wall_s,
                frames: frame_report,
                cpu: cpu_report,
                memory: memory_report,
                threads,
                terminal_renders_per_frame,
                errors,
                extra_events: extra,
                boot: boot_report,
            };

            if emit_json {
                println!(
                    "{}",
                    serde_json::to_string(&report).expect("serialize BenchReport")
                );
            } else {
                println!("{}", report.format_human());
            }

            // 7. 清理退场
            if let Some(setup) = setup_keepalive {
                setup.shell.update(cx, |s, cx| s.shutdown(cx));
            }
            for dir in temp_dirs {
                let _ = fs::remove_dir_all(&dir);
            }
            disable_frame_tracing();
            cx.update(|cx| cx.quit());
        })
        .detach();

        Self {
            layout,
            _subscriptions: subscriptions,
        }
    }
}

impl Render for Bench {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let root = div().size_full().bg(rgb(0x161616));
        match &self.layout {
            Layout::Split(side, terminals) => {
                let cell = |t: &Entity<GPuiFakeTerminal>| {
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
                root.bg(rgb(0xffffff))
                    .flex()
                    .flex_row()
                    .child(div().w(px(280.)).flex_none().h_full().child(side.clone()))
                    .child(right)
            }
            Layout::Whole(app) => root.bg(rgb(0xffffff)).child(app.clone()),
            Layout::Shell(shell) => root.child(shell.clone()),
            Layout::Grid(apps) => root
                .bg(rgb(0xffffff))
                .flex()
                .flex_row()
                .flex_wrap()
                .children(
                    apps.iter()
                        .map(|app| div().w(px(240.)).h(px(400.)).child(app.clone())),
                ),
            Layout::RealTerminals(terms) => match terms.as_slice() {
                [one] => root.child(one.clone()),
                many => root.flex().flex_col().children(many.chunks(2).map(|row| {
                    div()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .flex()
                        .flex_row()
                        .children(
                            row.iter()
                                .map(|t| div().flex_1().min_w_0().h_full().child(t.clone())),
                        )
                })),
            },
            Layout::TermAndConv(conv, term) => root
                .flex()
                .flex_row()
                .child(div().flex_1().min_w_0().h_full().child(conv.clone()))
                .child(div().flex_1().min_w_0().h_full().child(term.clone())),
        }
    }
}

async fn wait_oneshot(mut rx: oneshot::Receiver<()>, label: &str, cx: &mut gpui::AsyncApp) {
    let waited = Instant::now();
    loop {
        match rx.try_recv() {
            Ok(()) | Err(tokio::sync::oneshot::error::TryRecvError::Closed) => break,
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                assert!(
                    waited.elapsed() < Duration::from_secs(30),
                    "等待 {label} 初始化超时"
                );
                cx.background_executor()
                    .timer(Duration::from_millis(10))
                    .await;
            }
        }
    }
}

fn install_minimal_terminal_host(connector: Arc<ArcTerminalConnector>, cx: &mut App) {
    struct MinimalHost {
        connector: Arc<ArcTerminalConnector>,
    }

    impl api::Host for MinimalHost {
        fn machines(&self, _cx: &App) -> Vec<api::Machine> {
            vec![api::Machine {
                key: RuntimeKey::Local,
                name: "This Mac".into(),
                unavailable: None,
            }]
        }
        fn client(&self, _machine: &RuntimeKey, _cx: &App) -> Option<api::client::RuntimeClient> {
            None
        }
        fn connector(
            &self,
            machine: &RuntimeKey,
            _cx: &App,
        ) -> Option<Arc<dyn api::client::Connector>> {
            (*machine == RuntimeKey::Local)
                .then(|| Arc::clone(&self.connector) as Arc<dyn api::client::Connector>)
        }
        fn machine_online(&self, machine: &RuntimeKey, _cx: &App) -> bool {
            *machine == RuntimeKey::Local
        }
        fn connections(&self, _cx: &App) -> api::Connections {
            api::Connections::default()
        }
        fn view_shown(&self, _id: &str, _cx: &App) -> bool {
            true
        }
        fn view_laid_out(&self, _id: &str, _cx: &App) -> bool {
            true
        }
        fn main_window(&self, _cx: &App) -> Option<AnyWindowHandle> {
            None
        }
        fn window_active(&self, _cx: &App) -> bool {
            true
        }
        fn overlay_open(&self, _cx: &App) -> bool {
            false
        }
        fn path_style(
            &self,
            _machine: &RuntimeKey,
            _cx: &App,
        ) -> api::model::runtime_path::PathStyle {
            api::model::runtime_path::PathStyle::Posix
        }
        fn session_states(
            &self,
            _machine: &RuntimeKey,
            _cx: &App,
        ) -> Vec<api::model::daemon_state::DaemonSessionState> {
            Vec::new()
        }
        fn session_presence(
            &self,
            _machine: &RuntimeKey,
            _session_id: &str,
            _cx: &App,
        ) -> Option<api::model::daemon_state::RuntimePresence> {
            None
        }
        fn workspace_directory(
            &self,
            _machine: &RuntimeKey,
            _cx: &App,
        ) -> Option<api::model::workspace_directory::WorkspaceDirectory> {
            None
        }
        fn notification_permission(&self, _cx: &App) -> api::NotificationPermission {
            api::NotificationPermission::Unsupported
        }
    }

    smelt_ui::init(cx);
    api::set_locale("zh-CN", cx);
    api::install(MinimalHost { connector }, cx);
}

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
        view.set_fps_monitor(false);
        view.load(NshApp::Js(bundle));
    });
    app
}

// --- 合成假终端与纯 GPUI 对照组 ------------------------------------------------------------

static LINES: std::sync::LazyLock<Vec<SharedString>> = std::sync::LazyLock::new(|| {
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

fn terminal_element() -> AnyElement {
    TERMINAL_RENDERS.fetch_add(1, Ordering::Relaxed);
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

struct GPuiFakeTerminal;

impl Render for GPuiFakeTerminal {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        terminal_element()
    }
}

static VIEW_BACKED: AtomicBool = AtomicBool::new(false);

struct NativeTerminal {
    view: Option<Entity<GPuiFakeTerminal>>,
}

impl NativeTerminal {
    fn make(_: ComponentContext) -> Box<dyn NativeComponent> {
        Box::new(NativeTerminal { view: None })
    }
}

impl NativeComponent for NativeTerminal {
    fn render(&mut self, _: &mut Window, cx: &mut App) -> Option<AnyElement> {
        if VIEW_BACKED.load(Ordering::Relaxed) {
            let view = self
                .view
                .get_or_insert_with(|| cx.new(|_| GPuiFakeTerminal));
            return Some(view.clone().into_any_element());
        }
        Some(terminal_element())
    }

    fn set_prop(&mut self, _: &str, _: Option<&Value>, _: &mut Window, _: &mut App) {}
}

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

struct Spinner {
    start: Instant,
}

impl Render for Spinner {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        window.request_animation_frame();
        let phase = self.start.elapsed().as_secs_f32() % 1.;
        div()
            .size_3()
            .rounded_full()
            .bg(rgb(0x333333))
            .opacity(0.2 + 0.8 * phase)
    }
}
