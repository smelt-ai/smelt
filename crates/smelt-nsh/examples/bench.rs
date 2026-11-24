//! 量 NSH 嵌进 GPUI 的代价，不进 App。开一个 1200×800 的窗口，预热 3 秒，再量 6 秒，打印
//! 主线程和整个进程的 CPU、这几秒画了几帧、平均每帧主线程多少、每秒 CPU 多少、内存、线程数，
//! 然后退出。窗口被挡住时系统不让它出帧，所以比每帧的，不比每秒的。
//!
//! ```text
//! cargo run -j 2 --release -p smelt-nsh --example bench -- <场景> [块数]
//! ```
//!
//! 基线与合成场景：
//! - `idle`：左边 NSH 会话列表（不转），右边 GPUI 假终端，各是各的视图。
//! - `split`：同上，列表第一行在转（方案 1 / 3：每块一个 GPUI 视图）。
//! - `whole`：整窗一块 NSH，列表在转，假终端作原生组件嵌在里面（方案 2）。
//! - `split4`、`whole4`：同 `split`、`whole`，右边换成四块假终端（2×2），像真窗口里有几块重面板。
//! - `wholev`、`whole4v`：同 `whole`、`whole4`，但原生组件交出的是一个 GPUI 视图，不是每帧
//!   新建的元素，看 GPUI Fast 能不能沿用上一帧。
//! - `gpui`：参照，同样的列表用 GPUI 写，转圈是单独的小视图，右边 GPUI 假终端。
//! - `views N`：N 块不转的 NSH 列表，量每多一块的内存和线程。
//! - `chat-stream-idle` / `chat-stream-30` / `chat-stream`：200 条合成对话流探针（空闲 / 30Hz / 60Hz）。
//!
//! 真实 TSX 实现压测场景：
//! - `real-conv-idle`：真实 `<SmeltConversation>` + 200 条真实格式消息，就绪空闲态。
//! - `real-conv-sweep`：真实 `<SmeltConversation>` + 200 条消息，`thinking` 态仅跑 `<Composer>` 原生扫光动效。
//! - `real-conv-stream-30` / `real-conv-stream-60`：真实 `<SmeltConversation>` + 200 条消息 + 扫光 + 30Hz/60Hz 增量吐字。
//! - `real-boot`：真实主壳 + 23 个内置插件冷启动编译耗时 + 全量 VM 挂载后空闲代价。
//! - `real-state-storm`：真实主壳 + 23 个内置插件 + 出厂布局下 20Hz `smelt.state` 全量广播风暴。

use std::{
    fs,
    io::{BufRead as _, BufReader, Write as _},
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use gpui::{
    AnyElement, AnyWindowHandle, App, AppContext as _, Bounds, Context, Entity, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, WindowBounds,
    WindowOptions, div, px, rgb, size,
};
use nsh_core::element::description::SetDescription;
use nsh_platform_gpui::{
    App as NshApp, AppOptions, AppView, AssetBase, Bundle, ComponentContext, NativeComponent,
};
use serde_json::{Value, json};
use smelt_nsh::{
    BackdropHooks, MainShell, MainShellEvent, OutletTarget, PluginManifest, PluginSource,
    ShellConfig, ShellLabels, SourceDir, WindowCorners,
    plugins::{PluginHost, Plugins},
};
use smelt_plugin_api::{
    self as api, Host, HostEvent, Machine, NotificationPermission, PluginView, RuntimeKey,
    ViewHandle, ViewSpec,
    client::{Connector, RuntimeClient, RuntimeStream, protocol::control::ControlResponse},
    model::{
        daemon_state::{DaemonSessionState, RuntimePresence},
        runtime_path::PathStyle,
        workspace_directory::{WorkspaceConversation, WorkspaceDirectory, WorkspaceTerminal},
    },
};
use tokio::sync::oneshot;

const WARMUP: Duration = Duration::from_secs(3);
const MEASURE: Duration = Duration::from_secs(6);

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "split".to_owned());
    let count: usize = args.next().and_then(|n| n.parse().ok()).unwrap_or(1);
    log::set_logger(&Logger).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    if mode == "real-boot" {
        run_boot_compile_probe();
    }

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
        cx.open_window(options, |window, cx| {
            cx.new(|cx| Bench::new(&mode, count, window, cx))
        })
        .unwrap();
        cx.activate(true);
    });
}

/// NSH 画过的帧，从它每秒一行的统计里加起来（所以精度是秒）。
static NSH_FRAMES: AtomicU64 = AtomicU64::new(0);
/// 参照场景里转圈视图画过的帧。
static GPUI_FRAMES: AtomicU64 = AtomicU64::new(0);

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
                NSH_FRAMES.fetch_add(n, Ordering::Relaxed);
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
    /// 真实主壳（含 23 个内置插件）。
    Shell(Entity<MainShell>),
    /// 一格一块。
    Grid(Vec<Entity<AppView>>),
}

struct Bench {
    layout: Layout,
    _subscriptions: Vec<Subscription>,
}

impl Bench {
    fn new(mode: &str, count: usize, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = Vec::new();
        let mut temp_dirs: Vec<PathBuf> = Vec::new();
        let mut conv_errors: Option<Arc<AtomicUsize>> = None;
        let mut shell_errors: Option<Arc<AtomicUsize>> = None;
        let mut dir_revision: Option<Arc<AtomicU64>> = None;
        let mut ready_wait: Option<oneshot::Receiver<()>> = None;
        let mut shell_ref: Option<Entity<MainShell>> = None;
        let mut conv_stream: Option<ConvStreamDriver> = None;
        let mut boot_t0: Option<Instant> = None;

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
                vec![cx.new(|_| Terminal)],
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
            | "real-conv-stream-60" => {
                let kind = match mode {
                    "real-conv-idle" => RealConvMode::Idle,
                    "real-conv-sweep" => RealConvMode::Sweep,
                    "real-conv-stream-30" => RealConvMode::Stream30,
                    "real-conv-stream-60" => RealConvMode::Stream60,
                    _ => unreachable!(),
                };
                let setup = setup_real_conv(kind, window, cx);
                temp_dirs.push(setup.stage);
                conv_errors = Some(setup.errors);
                ready_wait = Some(setup.ready_rx);
                conv_stream = setup.stream;
                Layout::Whole(setup.app)
            }
            "real-boot" | "real-boot-staged" | "real-state-storm" => {
                let plugins_dir = if mode == "real-boot-staged" {
                    let staged = std::env::temp_dir()
                        .join(format!("smelt-nsh-bench-staged-{}", uuid::Uuid::new_v4()));
                    let repo_plugins =
                        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
                    let report = smelt_nsh::stage_builtin_plugins(&repo_plugins, &staged)
                        .expect("预编译内置插件");
                    println!(
                        "BOOT_STAGE: {}ms (total={}, script={}, theme_only={})",
                        report.elapsed_ms, report.total, report.script_plugins, report.theme_only
                    );
                    temp_dirs.push(staged.clone());
                    staged
                } else {
                    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins")
                };
                boot_t0 = Some(Instant::now());
                let setup = setup_real_shell(plugins_dir, window, cx);
                dir_revision = Some(setup.dir_revision);
                let shell = setup.shell;
                shell_ref = Some(shell.clone());
                let err_counter = Arc::new(AtomicUsize::new(0));
                shell_errors = Some(err_counter.clone());
                shell.update(cx, |s, cx| {
                    s.app_view().update(cx, |view, _| {
                        view.set_fps_monitor(true);
                        view.on_app_error(move |error| {
                            err_counter.fetch_add(1, Ordering::Relaxed);
                            eprintln!("[bench-shell-error] {error}");
                        });
                    });
                });
                let (boot_tx, boot_rx) = oneshot::channel::<()>();
                let boot_tx = Arc::new(Mutex::new(Some(boot_tx)));
                if shell.read(cx).initial_plugins_settled()
                    && let Some(tx) = boot_tx.lock().unwrap().take()
                {
                    let _ = tx.send(());
                }
                let sub = cx.subscribe(&shell, move |_, _, ev: &MainShellEvent, _| {
                    if matches!(ev, MainShellEvent::InitialReady)
                        && let Some(tx) = boot_tx.lock().unwrap().take()
                    {
                        let _ = tx.send(());
                    }
                });
                subscriptions.push(sub);
                ready_wait = Some(boot_rx);
                Layout::Shell(shell)
            }
            other => panic!("不认识的场景 {other}"),
        };

        let mode_owned = mode.to_owned();
        let label = format!("{mode} {count}");
        cx.spawn(async move |_, cx| {
            // 等待真实插件 / 主壳初始化就绪后再开始 3s 预热，避免把冷启动耗时混入稳态测量。
            if let Some(rx) = ready_wait {
                let waited = Instant::now();
                let mut rx = rx;
                loop {
                    match rx.try_recv() {
                        Ok(()) => break,
                        Err(tokio::sync::oneshot::error::TryRecvError::Closed) => break,
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

            if let Some(shell) = shell_ref.as_ref() {
                let wait_layout_start = Instant::now();
                let standard_state = loop {
                    if let Some(st) = cx.update(|cx| api::layout("smelt.layouts/standard", cx)) {
                        break st;
                    }
                    assert!(
                        wait_layout_start.elapsed() < Duration::from_secs(5),
                        "等待 smelt.layouts/standard 超时"
                    );
                    cx.background_executor()
                        .timer(Duration::from_millis(10))
                        .await;
                };
                let (plugin_count, plugin_errs) = shell.update(cx, |s, cx| {
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
                    let list = s.installed_plugins();
                    let errs = list.iter().filter(|p| p.error.is_some()).count();
                    (list.len(), errs)
                });
                if let Some(t0) = boot_t0
                    && (mode_owned == "real-boot" || mode_owned == "real-boot-staged")
                {
                    let ready_ms = t0.elapsed().as_secs_f64() * 1000.0;
                    let runtime_errs = shell_errors
                        .as_ref()
                        .map_or(0, |c| c.load(Ordering::Relaxed));
                    println!(
                        "BOOT_READY: {ready_ms:.1}ms (plugins={plugin_count}, plugin_errors={plugin_errs}, runtime_errors={runtime_errs})"
                    );
                }
                if let Some(counter) = shell_errors.as_ref() {
                    counter.fetch_add(plugin_errs, Ordering::Relaxed);
                }
            }

            // 对真实流式场景启动独立后台定时推送（先预热 `TextView` / QuickJS 再采 6s 稳态）。
            let stream_stop = Arc::new(AtomicBool::new(false));
            if let Some(driver) = conv_stream {
                let stop = stream_stop.clone();
                cx.spawn(async move |cx| {
                    let chunks = conv_stream_chunks();
                    let sub_id = driver
                        .sub_id
                        .lock()
                        .unwrap()
                        .clone()
                        .expect("acp.subscribe 应已写入 sub_id");
                    let mut chunk_ix = 0usize;
                    let mut rev = 2u64;
                    let mut acc = String::new();
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
                                    "entries_offset": 199,
                                    "entries_total": 200,
                                    "snapshot_revision": rev,
                                    "entries": [{
                                        "Assistant": { "text": acc, "thought": false }
                                    }],
                                },
                            }),
                        );
                    }
                })
                .detach();
            }

            cx.background_executor().timer(WARMUP).await;

            let measure_t0 = Instant::now();
            let before = Usage::now();
            let mut broadcasts = 0usize;
            if mode_owned == "real-state-storm" {
                let rev_counter = dir_revision.expect("dir_revision");
                while measure_t0.elapsed() < MEASURE {
                    cx.background_executor()
                        .timer(Duration::from_millis(50))
                        .await;
                    rev_counter.fetch_add(1, Ordering::Relaxed);
                    broadcasts += 1;
                    cx.update(|cx| api::emit(HostEvent::SessionsChanged, cx));
                }
            } else {
                cx.background_executor().timer(MEASURE).await;
            }
            let after = Usage::now();
            stream_stop.store(true, Ordering::Relaxed);
            let seconds = measure_t0.elapsed().as_secs_f64().max(0.001);

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

            let mut extra = String::new();
            if let Some(errs) = conv_errors.as_ref().or(shell_errors.as_ref()) {
                extra.push_str(&format!(", errors={}", errs.load(Ordering::Relaxed)));
            }
            if broadcasts > 0 {
                extra.push_str(&format!(", broadcasts={broadcasts}"));
            }

            println!(
                "RESULT {label}: 帧 {frames}，每帧主线程 {:.2} ms、进程 {:.2} ms，每帧终端 render {:.1} 次；主线程 {:.1} ms/s、进程 {:.1} ms/s，内存 {rss_mb:.0} MB，线程 {threads}{extra}",
                per_frame(after.main - before.main),
                per_frame(after.process - before.process),
                (after.terminal - before.terminal) as f64 / frames.max(1) as f64,
                (after.main - before.main) * 1000. / seconds,
                (after.process - before.process) * 1000. / seconds,
            );

            if let Some(shell) = shell_ref {
                shell.update(cx, |s, cx| s.shutdown(cx));
            }
            for dir in temp_dirs {
                let _ = fs::remove_dir_all(&dir);
            }
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

// --- 真实 <SmeltConversation> 压测底座 -----------------------------------------------------

#[derive(Clone, Copy)]
enum RealConvMode {
    Idle,
    Sweep,
    Stream30,
    Stream60,
}

impl RealConvMode {
    fn stream_interval(self) -> Option<Duration> {
        match self {
            Self::Idle | Self::Sweep => None,
            Self::Stream30 => Some(Duration::from_millis(33)),
            Self::Stream60 => Some(Duration::from_millis(16)),
        }
    }

    fn active(self) -> bool {
        !matches!(self, Self::Idle)
    }
}

struct ConvStreamDriver {
    host: PluginHost,
    sub_id: Arc<Mutex<Option<String>>>,
    interval: Duration,
}

struct RealConvSetup {
    app: Entity<AppView>,
    stage: PathBuf,
    errors: Arc<AtomicUsize>,
    ready_rx: oneshot::Receiver<()>,
    stream: Option<ConvStreamDriver>,
}

struct WindowDragStub;

impl WindowDragStub {
    fn make(_: ComponentContext) -> Box<dyn NativeComponent> {
        Box::new(WindowDragStub)
    }
}

impl NativeComponent for WindowDragStub {
    fn render(&mut self, _: &mut Window, _: &mut App) -> Option<AnyElement> {
        Some(div().size_full().into_any_element())
    }

    fn set_prop(&mut self, _: &str, _: Option<&Value>, _: &mut Window, _: &mut App) {}
}

const CONV_SHELL: &str = r#"
import { View } from "nsh";
import { Slot } from "./slots.tsx";

export function Shell() {
  return (
    <View style={{ flex: 1 }}>
      <Slot id="probe" style={{ flex: 1 }} />
    </View>
  );
}
"#;

const CONV_PLUGIN: &str = r##"
import { View } from "nsh";
import { SmeltConversation } from "smelt/sdk";

const DIR_ACP = {
  sid: "acp-bench-1",
  agent: "pi",
  cwd: "/repo",
  launch: { command: "pi", env: {} },
  history_session_id: null,
  config_values: [],
  pending_prompt: null,
  pending_agent_preset: null,
  agent_definition_id: null,
  session_title: "Bench Conversation",
};

export default function Probe() {
  return (
    <View style={{ flex: 1, width: "100%", height: "100%", backgroundColor: "#161616" }}>
      <SmeltConversation
        machine={null}
        sessionId="acp-bench-1"
        directoryAcp={DIR_ACP}
        embedded={false}
        title="对话性能测试"
      />
    </View>
  );
}
"##;

const CONV_STREAM_BODY: &str = r#"## 实时性能采样分析

正在核对 **SmeltConversation** 在 200 条真实消息下的渲染热路径：

1. **虚拟列表隔离**：历史条目由 `List` 按视口回收，只重绘最新流式节点。
2. **局部信号**：`sameShapeTailOnly` 路径直接写 `streamingText`，避免重建 `displayRows`。
3. **扫光动效**：输入框顶边采用原生 `Animated.keyframes` 在 UI 线程往返移动，不经 QuickJS 回调。

```rust
pub fn apply_snapshot(delta: &SnapshotDelta) -> bool {
    delta.entries_offset + 1 == delta.entries_total
}
```

| 观测项 | 采样周期 | 关注重点 |
| --- | ---: | --- |
| 主线程 CPU | 6 s | 布局与 GPUI Paint 开销 |
| 进程总 CPU | 6 s | 包含 QuickJS 解析与状态广播 |
| 帧耗时 | 每帧 | 目标 60Hz 不抖动 |

本轮压测继续补充下一段代码块前后的表格排版与字距稳定性校验。
"#;

fn conv_stream_chunks() -> Vec<String> {
    let mut chunks = Vec::new();
    let mut cur = String::new();
    for ch in CONV_STREAM_BODY.chars() {
        cur.push(ch);
        if cur.chars().count() >= 6 {
            chunks.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        chunks.push(cur);
    }
    chunks
}

fn build_conv_entries(tail_text: &str) -> Vec<Value> {
    let mut entries = Vec::with_capacity(200);
    for i in 0..199 {
        match i % 4 {
            0 => entries.push(json!({
                "User": format!("第 {} 问：核对真实 Conversation 插件第 {i} 条历史气泡、折叠过程与流式布局开销？", i / 4 + 1),
            })),
            1 => entries.push(json!({
                "ToolCall": {
                    "id": format!("tc-read-{i}"),
                    "title": format!("crates/smelt-nsh/src/plugins/sdk.tsx"),
                    "kind": "read",
                    "status": "completed",
                    "output": [{ "Text": format!("Read 120 lines around segment {i}") }],
                    "children": [],
                }
            })),
            2 => entries.push(json!({
                "ToolCall": {
                    "id": format!("tc-edit-{i}"),
                    "title": "crates/smelt-nsh/src/plugins/conversation/composer.tsx",
                    "kind": "edit",
                    "status": "completed",
                    "output": [{
                        "Diff": {
                            "path": "/repo/crates/smelt-nsh/src/plugins/conversation/composer.tsx",
                            "old_text": "let old = 1;\n",
                            "new_text": format!("let next = {i};\nlet sweep = true;\n"),
                        }
                    }],
                    "children": [],
                }
            })),
            _ => entries.push(json!({
                "Assistant": {
                    "text": format!(
                        "已完成条目 #{i} 检查：\n\n```rust\nfn step_{i}(n: usize) -> usize {{ n + {i} }}\n```\n\n- 虚拟列表按可见区回收\n- 工具差异标签可点击弹窗查看"
                    ),
                    "thought": false,
                }
            })),
        }
    }
    entries.push(json!({
        "Assistant": {
            "text": tail_text,
            "thought": false,
        }
    }));
    entries
}

fn setup_real_conv(
    mode: RealConvMode,
    window: &mut Window,
    cx: &mut Context<Bench>,
) -> RealConvSetup {
    let root = std::env::temp_dir().join(format!("smelt-nsh-bench-conv-{}", uuid::Uuid::new_v4()));
    let _ = fs::remove_dir_all(&root);

    let host = PluginHost::new();
    let errors = Arc::new(AtomicUsize::new(0));
    let (ready_tx, ready_rx) = oneshot::channel::<()>();
    let ready_tx = Arc::new(Mutex::new(Some(ready_tx)));
    let shared_sub_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    host.handle("state", |_plugin, call| {
        call.resolve(json!({
            "revision": 1,
            "locale": "zh-CN",
            "window_active": true,
            "machines": [],
        }));
    });
    host.handle("call", |_plugin, call| {
        let data = call.data();
        if data["method"].as_str() == Some("session.launch.prepare") {
            let launch = data["params"]["launch"].clone();
            call.resolve(json!({
                "launch": launch,
                "skills": [],
                "mcp_servers": [],
            }));
        } else {
            call.resolve(Value::Null);
        }
    });
    host.handle("set_active_context", |_plugin, call| {
        call.resolve(Value::Null)
    });
    host.handle("acp.action", |_plugin, call| call.resolve(Value::Null));
    host.handle("acp.unsubscribe", |_plugin, call| call.resolve(Value::Null));

    {
        let sub_host = host.clone();
        let ready_tx = ready_tx.clone();
        let shared_sub_id = shared_sub_id.clone();
        host.handle("acp.subscribe", move |_plugin, call| {
            let sub_id = call.data()["sub_id"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            call.resolve(Value::Null);

            let phase = if mode.active() { "thinking" } else { "ready" };
            let tail_text = if mode.stream_interval().is_some() {
                ""
            } else {
                CONV_STREAM_BODY
            };
            sub_host.send_to(
                "probe",
                "smelt.acp.event",
                json!({
                    "sub_id": sub_id,
                    "kind": "snapshot",
                    "snapshot": {
                        "phase": phase,
                        "replaying_history": false,
                        "entries": build_conv_entries(tail_text),
                        "entries_offset": 0,
                        "entries_total": 200,
                        "snapshot_revision": 1,
                        "config_options": [{
                            "config_id": "model",
                            "current_value": "anthropic/claude-sonnet-4",
                            "current_name": "Claude Sonnet 4",
                            "options": [["anthropic/claude-sonnet-4", "Claude Sonnet 4"]],
                        }],
                        "model": {
                            "config_id": "model",
                            "current_value": "anthropic/claude-sonnet-4",
                            "current_name": "Claude Sonnet 4",
                            "options": [["anthropic/claude-sonnet-4", "Claude Sonnet 4"]],
                        },
                        "usage": [24_000, 200_000],
                    },
                }),
            );
            *shared_sub_id.lock().unwrap() = Some(sub_id);
            if let Some(tx) = ready_tx.lock().unwrap().take() {
                let _ = tx.send(());
            }
        });
    }

    let app = cx.new(|cx| AppView::new(AppOptions::default(), window, cx));
    {
        let err_host = host.clone();
        let err_counter = errors.clone();
        app.update(cx, |view, _| {
            view.register_component("smelt.window-drag", Rc::new(WindowDragStub::make));
            view.set_fps_monitor(true);
            view.on_app_error(move |error| {
                err_counter.fetch_add(1, Ordering::Relaxed);
                eprintln!("[bench-conv-error] {error}");
                err_host.on_app_error(error);
            });
        });
    }

    let ui_sets = vec![
        SetDescription::from(nsh_base::registry()),
        SetDescription::from(smelt_nsh::testing::host_registry()),
    ];
    let mut plugins = Plugins::new(&root.join("stage"), CONV_SHELL, ui_sets).unwrap();
    let plugin_dir = root.join("sources/probe");
    fs::create_dir_all(&plugin_dir).unwrap();
    fs::write(plugin_dir.join("main.tsx"), CONV_PLUGIN).unwrap();
    plugins.install("probe", &plugin_dir).unwrap();

    let start = plugins.start();
    app.update(cx, |view, _| host.apply(view, start));

    let stream = mode.stream_interval().map(|interval| ConvStreamDriver {
        host,
        sub_id: shared_sub_id,
        interval,
    });

    RealConvSetup {
        app,
        stage: root,
        errors,
        ready_rx,
        stream,
    }
}

// --- 真实全量主壳 + 23 内置插件压测底座 -----------------------------------------------------

fn run_boot_compile_probe() {
    let total_t0 = Instant::now();
    let stage =
        std::env::temp_dir().join(format!("smelt-nsh-bench-compile-{}", uuid::Uuid::new_v4()));
    let ui_sets = vec![
        SetDescription::from(nsh_base::registry()),
        SetDescription::from(smelt_nsh::testing::host_registry()),
    ];
    let t0 = Instant::now();
    let mut plugins =
        Plugins::new(&stage, include_str!("../shell/MainShell.tsx"), ui_sets).unwrap();
    let shell_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let repo_plugins = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
    let mut entries: Vec<PathBuf> = fs::read_dir(&repo_plugins)
        .expect("读取 plugins/")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.join(smelt_nsh::MANIFEST_FILE).is_file())
        .collect();
    entries.sort();

    let mut per_plugin: Vec<(String, f64)> = Vec::new();
    let mut theme_only = 0usize;
    let plugins_t0 = Instant::now();
    for root in entries {
        let manifest = PluginManifest::read(&root).expect("读取 smelt-plugin.json");
        if manifest.is_theme_only() {
            theme_only += 1;
            continue;
        }
        let pt0 = Instant::now();
        plugins
            .install_entry(&manifest.id, &root, &manifest.entry)
            .expect("编译内置插件");
        per_plugin.push((manifest.id, pt0.elapsed().as_secs_f64() * 1000.0));
    }
    let plugins_ms = plugins_t0.elapsed().as_secs_f64() * 1000.0;
    let total_ms = total_t0.elapsed().as_secs_f64() * 1000.0;
    per_plugin.sort_by(|a, b| b.1.total_cmp(&a.1));
    let top5: Vec<String> = per_plugin
        .iter()
        .take(5)
        .map(|(id, ms)| format!("{id}={ms:.1}ms"))
        .collect();
    let _ = fs::remove_dir_all(&stage);
    println!(
        "BOOT_COMPILE: total={total_ms:.1}ms (MainShell={shell_ms:.1}ms, {} script plugins={plugins_ms:.1}ms, theme_only={theme_only}); slowest=[{}]",
        per_plugin.len(),
        top5.join(", ")
    );
}

struct BenchHost {
    dir_revision: Arc<AtomicU64>,
}

impl BenchHost {
    fn new(dir_revision: Arc<AtomicU64>) -> Self {
        Self { dir_revision }
    }
}

impl Host for BenchHost {
    fn machines(&self, _cx: &App) -> Vec<Machine> {
        vec![Machine {
            key: RuntimeKey::Local,
            name: "This Mac".into(),
            unavailable: None,
        }]
    }

    fn client(&self, machine: &RuntimeKey, _cx: &App) -> Option<RuntimeClient> {
        if *machine != RuntimeKey::Local {
            return None;
        }
        Some(RuntimeClient::new(move || {
            let (client, server) = RuntimeStream::pair()?;
            std::thread::spawn(move || {
                let mut line = String::new();
                if BufReader::new(server.try_clone().unwrap())
                    .read_line(&mut line)
                    .is_err()
                {
                    return;
                }
                let Ok(req) = serde_json::from_str::<Value>(&line) else {
                    return;
                };
                let request_id = req["request_id"].as_str().unwrap_or("req");
                let method = req["method"].as_str().unwrap_or("");
                let payload = match method {
                    "quota.get" | "quota.refresh" => json!({
                        "targets": ["claude"],
                        "statuses": [[
                            "claude",
                            {
                                "quota": {
                                    "windows": {
                                        "windows": [{
                                            "label": "5h",
                                            "used_percent": 32.0,
                                            "resets_at_ms": null,
                                            "reset_after_seconds": 3600,
                                            "scope": null,
                                        }]
                                    }
                                },
                                "checked_at_ms": 1_700_000_000_000i64,
                                "error": null,
                            }
                        ]]
                    }),
                    "quota.settings.get" => json!({
                        "providers": [
                            { "target": "claude", "enabled": true, "keychain_denied": false }
                        ]
                    }),
                    "remote.config.get" => json!({
                        "enabled": false,
                        "iroh_relay": ""
                    }),
                    "new_session.menu.get" => json!({
                        "sections": { "common": [], "terminal": [], "conversation": [] }
                    }),
                    _ => json!({}),
                };
                let resp = serde_json::to_value(ControlResponse::ok(request_id, payload)).unwrap();
                let _ = writeln!(&server, "{resp}");
            });
            Ok(client)
        }))
    }

    fn connector(&self, _machine: &RuntimeKey, _cx: &App) -> Option<Arc<dyn Connector>> {
        None
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

    fn path_style(&self, _machine: &RuntimeKey, _cx: &App) -> PathStyle {
        PathStyle::Posix
    }

    fn session_states(&self, _machine: &RuntimeKey, _cx: &App) -> Vec<DaemonSessionState> {
        Vec::new()
    }

    fn session_presence(
        &self,
        _machine: &RuntimeKey,
        _session_id: &str,
        _cx: &App,
    ) -> Option<RuntimePresence> {
        None
    }

    fn workspace_directory(&self, _machine: &RuntimeKey, _cx: &App) -> Option<WorkspaceDirectory> {
        let rev = self.dir_revision.load(Ordering::Relaxed);
        let conversations = (0..30)
            .map(|i| WorkspaceConversation {
                id: format!("conv-{i}"),
                custom_title: Some(format!("Session #{i}: NSH performance benchmark")),
                last_updated_at: 1_700_000_000 + i as i64 + if i == 0 { rev as i64 } else { 0 },
                terminals: vec![WorkspaceTerminal {
                    id: Some(format!("t-{i}")),
                    cwd: Some(
                        if i % 2 == 0 {
                            "/work/smelt"
                        } else {
                            "/work/other"
                        }
                        .into(),
                    ),
                    ..Default::default()
                }],
                ..Default::default()
            })
            .collect();
        Some(WorkspaceDirectory {
            epoch: "1".into(),
            revision: 1 + rev,
            projects: vec!["/work/smelt".into(), "/work/other".into()],
            repositories: Vec::new(),
            worksets: Vec::new(),
            conversations,
        })
    }

    fn notification_permission(&self, _cx: &App) -> NotificationPermission {
        NotificationPermission::Unsupported
    }
}

struct StubView;

impl Render for StubView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

impl PluginView for StubView {
    fn open(&mut self, _: &Value, _: &mut Window, _: &mut Context<Self>) {}
}

fn stub_spec(name: &str) -> ViewSpec {
    ViewSpec {
        name: name.to_owned().into(),
        title: name.to_owned().into(),
        icon: "icons/layout-dashboard.svg".into(),
        draws_top_bar: true,
        settings: None,
        instances: false,
        fills: Vec::new(),
        build: Rc::new(|_, cx| -> Rc<dyn ViewHandle> { Rc::new(cx.new(|_| StubView)) }),
    }
}

struct RealShellSetup {
    shell: Entity<MainShell>,
    dir_revision: Arc<AtomicU64>,
}

fn setup_real_shell(
    plugins_dir: PathBuf,
    window: &mut Window,
    cx: &mut Context<Bench>,
) -> RealShellSetup {
    let dir_revision = Arc::new(AtomicU64::new(0));
    let host_rev = dir_revision.clone();
    smelt_ui::init(cx);
    api::set_locale("zh-CN", cx);
    api::install(BenchHost::new(host_rev), cx);
    api::register_views(
        "smelt",
        vec![stub_spec("outlet"), stub_spec("settings")],
        cx,
    )
    .unwrap();

    let shell = cx.new(|cx| {
        MainShell::new(
            ShellConfig {
                sources: vec![SourceDir {
                    path: plugins_dir,
                    source: PluginSource::Builtin,
                    watch: false,
                }],
                backdrop: BackdropHooks::default(),
                corners: WindowCorners {
                    lights: px(78.),
                    rounded: px(10.),
                },
                padding: 0.,
                labels: ShellLabels::default(),
            },
            window,
            cx,
        )
    });
    RealShellSetup {
        shell,
        dir_revision,
    }
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

static TERMINAL_RENDERS: AtomicU64 = AtomicU64::new(0);

/// 60 行、每行 120 个等宽字，代替真终端。
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

/// 方案 1 / 3：终端是自己的 GPUI 视图。
struct Terminal;

impl Render for Terminal {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        terminal_element()
    }
}

/// 原生组件交出 GPUI 视图（、），不每帧新建元素。
static VIEW_BACKED: AtomicBool = AtomicBool::new(false);

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
        if VIEW_BACKED.load(Ordering::Relaxed) {
            let view = self.view.get_or_insert_with(|| cx.new(|_| Terminal));
            return Some(view.clone().into_any_element());
        }
        Some(terminal_element())
    }

    fn set_prop(&mut self, _: &str, _: Option<&Value>, _: &mut Window, _: &mut App) {}
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
        GPUI_FRAMES.fetch_add(1, Ordering::Relaxed);
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
            terminal: TERMINAL_RENDERS.load(Ordering::Relaxed),
            frames: NSH_FRAMES.load(Ordering::Relaxed) + GPUI_FRAMES.load(Ordering::Relaxed),
        }
    }
}
