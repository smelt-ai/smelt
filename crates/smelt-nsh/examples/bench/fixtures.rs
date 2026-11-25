//! 真实插件压测底座：`BenchHost`、模拟控制 RPC（配额/Git/文件树/终端连接器）、真实 `<SmeltConversation>`
//! 与真实全量 `MainShell` 组装逻辑。

use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    fs,
    io::{BufRead as _, BufReader, Write as _},
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use gpui::{
    AnyElement, AnyWindowHandle, App, AppContext as _, Context, Entity, IntoElement, Render,
    Subscription, Window, div, px,
};
use nsh_core::element::description::SetDescription;
use nsh_platform_gpui::{AppOptions, AppView, ComponentContext, NativeComponent};
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
use smelt_ui::markdown_mermaid;
use tokio::sync::oneshot;

use crate::metrics::BootPluginTiming;
use crate::terminal::ArcTerminalConnector;

pub struct WindowDragStub;

impl WindowDragStub {
    pub fn make(_: ComponentContext) -> Box<dyn NativeComponent> {
        Box::new(WindowDragStub)
    }
}

impl NativeComponent for WindowDragStub {
    fn render(&mut self, _: &mut Window, _: &mut App) -> Option<AnyElement> {
        use gpui::Styled as _;
        Some(div().size_full().into_any_element())
    }

    fn set_prop(&mut self, _: &str, _: Option<&Value>, _: &mut Window, _: &mut App) {}
}

// --- 真实 <SmeltConversation> 压测底座 -----------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RealConvMode {
    Idle,
    Sweep,
    Stream30,
    Stream60,
    Grow,
    Scroll,
}

impl RealConvMode {
    pub fn stream_interval(self) -> Option<Duration> {
        match self {
            Self::Idle | Self::Sweep | Self::Scroll => None,
            Self::Stream30 | Self::Grow => Some(Duration::from_millis(33)),
            Self::Stream60 => Some(Duration::from_millis(16)),
        }
    }

    pub fn active(self) -> bool {
        !matches!(self, Self::Idle | Self::Scroll)
    }

    pub fn grows(self) -> bool {
        matches!(self, Self::Grow)
    }

    pub fn initial_entry_count(self) -> usize {
        match self {
            Self::Grow => 100,
            _ => 200,
        }
    }
}

pub struct ConvStreamDriver {
    pub host: PluginHost,
    pub sub_id: Arc<Mutex<Option<String>>>,
    pub interval: Duration,
    pub grows: bool,
    pub initial_count: usize,
}

pub struct RealConvSetup {
    pub app: Entity<AppView>,
    pub stage: PathBuf,
    pub errors: Arc<AtomicUsize>,
    pub ready_rx: oneshot::Receiver<()>,
    pub stream: Option<ConvStreamDriver>,
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
import { SmeltConversation } from "smelt/conversation";

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

pub const CONV_STREAM_BODY: &str = r#"## 实时性能采样分析

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

pub fn conv_stream_chunks() -> Vec<String> {
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

pub fn make_conv_tool_entry(i: usize) -> Value {
    if i.is_multiple_of(2) {
        json!({
            "ToolCall": {
                "id": format!("tc-read-{i}"),
                "title": "crates/smelt-nsh/src/plugins/sdk.tsx",
                "kind": "read",
                "status": "completed",
                "output": [{ "Text": format!("Read 120 lines around segment {i}") }],
                "children": [],
            }
        })
    } else {
        json!({
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
        })
    }
}

pub fn make_conv_assistant_entry(i: usize, with_mermaid: bool) -> Value {
    let body = if with_mermaid {
        format!(
            "已完成条目 #{i} 结构核查：\n\n```mermaid\nflowchart LR\n  ACP[ACP Delta {i}] --> NSH[Virtual List]\n  NSH --> GPUI[GPUI Paint]\n```\n\n- 虚拟列表按可见区回收\n- Mermaid 图表通过 `mermaid.render` 异步光栅化"
        )
    } else {
        format!(
            "已完成条目 #{i} 检查：\n\n```rust\nfn step_{i}(n: usize) -> usize {{ n + {i} }}\n```\n\n- 虚拟列表按可见区回收\n- 工具差异标签可点击弹窗查看"
        )
    };
    json!({
        "Assistant": {
            "text": body,
            "thought": false,
        }
    })
}

fn build_conv_entries(total_count: usize, tail_text: &str) -> Vec<Value> {
    let prefix = total_count.saturating_sub(1);
    let mut entries = Vec::with_capacity(total_count);
    for i in 0..prefix {
        match i % 4 {
            0 => entries.push(json!({
                "User": format!(
                    "第 {} 问：核对真实 Conversation 插件第 {i} 条历史气泡、折叠过程与流式布局开销？",
                    i / 4 + 1
                ),
            })),
            1 | 2 => entries.push(make_conv_tool_entry(i)),
            _ => entries.push(make_conv_assistant_entry(i, i % 20 == 19)),
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

pub fn setup_real_conv(
    mode: RealConvMode,
    window: &mut Window,
    cx: &mut Context<impl Render>,
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
            "theme": smelt_nsh::testing::theme_state(16.),
            "window_active": true,
            "machines_meta": [],
            "sessions_data": [],
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
    host.handle("mermaid.render", |_plugin, call| {
        let Some(source) = call.data()["source"].as_str().map(str::to_owned) else {
            return call.reject("mermaid.render：缺 source 字符串");
        };
        api::blocking::spawn_detached(move || match markdown_mermaid::render_mermaid_svg_data_uri(
            &source,
        ) {
            Ok(rendered) => call.resolve(json!({
                "ok": true,
                "data_uri": rendered.data_uri,
                "width": rendered.width,
                "height": rendered.height,
            })),
            Err(error) => call.resolve(json!({
                "ok": false,
                "error": error,
            })),
        });
    });
    host.handle("set_active_context", |_plugin, call| {
        call.resolve(Value::Null)
    });
    host.handle("acp.action", |_plugin, call| call.resolve(Value::Null));
    host.handle("acp.unsubscribe", |_plugin, call| call.resolve(Value::Null));

    let initial_count = mode.initial_entry_count();
    {
        let sub_host = host.clone();
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
                        "entries": build_conv_entries(initial_count, tail_text),
                        "entries_offset": 0,
                        "entries_total": initial_count,
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
            *shared_sub_id.lock().expect("sub_id lock poisoned") = Some(sub_id);
            let tx = ready_tx.lock().expect("ready_tx lock poisoned").take();
            if let Some(tx) = tx {
                let _ = tx.send(());
            }
        });
    }

    let app = cx.new(|cx| {
        AppView::new(
            AppOptions {
                inline_root: true,
                ..AppOptions::default()
            },
            window,
            cx,
        )
    });
    {
        let err_host = host.clone();
        let err_counter = errors.clone();
        app.update(cx, |view, _| {
            view.register_component("smelt.window-drag", Rc::new(WindowDragStub::make));
            view.set_fps_monitor(false);
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
    let repo_plugins = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
    plugins.set_modules(smelt_nsh::discover_module_packages(&[SourceDir {
        path: repo_plugins,
        source: PluginSource::Builtin,
        watch: false,
    }]));
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
        grows: mode.grows(),
        initial_count,
    });

    RealConvSetup {
        app,
        stage: root,
        errors,
        ready_rx,
        stream,
    }
}

// --- 真实全量主壳 + 内置插件压测底座 -----------------------------------------------------

pub struct BootCompileResult {
    pub compile_total_ms: f64,
    pub compile_shell_ms: f64,
    pub compile_plugins_ms: f64,
    pub slowest_plugins: Vec<BootPluginTiming>,
}

pub fn run_boot_compile_probe() -> BootCompileResult {
    let total_t0 = Instant::now();
    let stage =
        std::env::temp_dir().join(format!("smelt-nsh-bench-compile-{}", uuid::Uuid::new_v4()));
    let ui_sets = vec![
        SetDescription::from(nsh_base::registry()),
        SetDescription::from(smelt_nsh::testing::host_registry()),
    ];
    let t0 = Instant::now();
    let mut plugins =
        Plugins::new(&stage, include_str!("../../shell/MainShell.tsx"), ui_sets).unwrap();
    let shell_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let repo_plugins = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
    plugins.set_modules(smelt_nsh::discover_module_packages(&[SourceDir {
        path: repo_plugins.clone(),
        source: PluginSource::Builtin,
        watch: false,
    }]));
    let mut entries: Vec<PathBuf> = fs::read_dir(&repo_plugins)
        .expect("读取 plugins/")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.join(smelt_nsh::MANIFEST_FILE).is_file())
        .collect();
    entries.sort();

    let mut per_plugin: Vec<(String, f64)> = Vec::new();
    let mut theme_only = 0usize;
    let mut layout_only = 0usize;
    let mut module_plugins = 0usize;
    let plugins_t0 = Instant::now();
    for root in entries {
        let manifest = PluginManifest::read(&root).expect("读取 smelt-plugin.json");
        if manifest.is_theme_only() {
            theme_only += 1;
            continue;
        }
        if manifest.is_layout_only() {
            layout_only += 1;
            continue;
        }
        if manifest.is_module() {
            module_plugins += 1;
            continue;
        }
        let pt0 = Instant::now();
        plugins
            .install_entry(&manifest.id, &root, &manifest.entry, &manifest.requires)
            .expect("编译内置插件");
        per_plugin.push((manifest.id, pt0.elapsed().as_secs_f64() * 1000.0));
    }
    let plugins_ms = plugins_t0.elapsed().as_secs_f64() * 1000.0;
    let total_ms = total_t0.elapsed().as_secs_f64() * 1000.0;
    per_plugin.sort_by(|a, b| b.1.total_cmp(&a.1));
    let slowest_plugins: Vec<BootPluginTiming> = per_plugin
        .iter()
        .take(5)
        .map(|(id, ms)| BootPluginTiming {
            id: id.clone(),
            compile_ms: *ms,
        })
        .collect();
    let top5_str: Vec<String> = slowest_plugins
        .iter()
        .map(|item| format!("{}={:.1}ms", item.id, item.compile_ms))
        .collect();
    let _ = fs::remove_dir_all(&stage);
    eprintln!(
        "BOOT_COMPILE: total={total_ms:.1}ms (MainShell={shell_ms:.1}ms, {} script plugins={plugins_ms:.1}ms, module={module_plugins}, theme_only={theme_only}, layout_only={layout_only}); slowest=[{}]",
        per_plugin.len(),
        top5_str.join(", ")
    );
    BootCompileResult {
        compile_total_ms: total_ms,
        compile_shell_ms: shell_ms,
        compile_plugins_ms: plugins_ms,
        slowest_plugins,
    }
}

#[derive(Default)]
pub struct BenchRpcCounters {
    pub git_diff_all_served: AtomicBool,
    pub file_open_served: AtomicBool,
    pub project_tree_served: AtomicBool,
}

#[derive(Clone)]
pub struct BenchHostState {
    pub dir_revision: Arc<AtomicU64>,
    pub shown_views: Rc<RefCell<HashSet<String>>>,
    pub laid_out_views: Rc<RefCell<HashSet<String>>>,
    pub main_window: Rc<RefCell<Option<AnyWindowHandle>>>,
    pub connector: Option<Arc<ArcTerminalConnector>>,
    pub rpc_counters: Arc<BenchRpcCounters>,
    /// 前几个模拟会话处于运行中（见 [`ShellOptions::running`]）。
    pub running: usize,
}

impl BenchHostState {
    pub fn new(
        connector: Option<Arc<ArcTerminalConnector>>,
        main_window: Option<AnyWindowHandle>,
    ) -> Self {
        Self {
            dir_revision: Arc::new(AtomicU64::new(0)),
            shown_views: Rc::new(RefCell::new(HashSet::new())),
            laid_out_views: Rc::new(RefCell::new(HashSet::new())),
            main_window: Rc::new(RefCell::new(main_window)),
            connector,
            rpc_counters: Arc::new(BenchRpcCounters::default()),
            running: 0,
        }
    }
}

/// 将 `MainShell::shell_view` 的真实可见与摆位集合同步到 `BenchHostState`，
/// 并按需发射 `ShownViewsChanged` / `LaidOutViewsChanged`，避免隐藏插件后台空转采样。
pub fn sync_shell_visibility(
    view: smelt_nsh::ComputedShellView,
    state: &BenchHostState,
    cx: &mut App,
) {
    let shown_changed = {
        let mut cur = state.shown_views.borrow_mut();
        if *cur != view.shown {
            *cur = view.shown;
            true
        } else {
            false
        }
    };
    let laid_out_changed = {
        let mut cur = state.laid_out_views.borrow_mut();
        if *cur != view.laid_out {
            *cur = view.laid_out;
            true
        } else {
            false
        }
    };
    if shown_changed {
        api::emit(HostEvent::ShownViewsChanged, cx);
    }
    if laid_out_changed {
        api::emit(HostEvent::LaidOutViewsChanged, cx);
    }
}

pub struct BenchHost {
    state: BenchHostState,
}

impl BenchHost {
    pub fn new(state: BenchHostState) -> Self {
        Self { state }
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
        let counters = Arc::clone(&self.state.rpc_counters);
        Some(RuntimeClient::new(move || {
            let (client, server) = RuntimeStream::pair()?;
            let counters = Arc::clone(&counters);
            std::thread::spawn(move || {
                let mut line = String::new();
                if BufReader::new(server.try_clone().expect("clone server stream"))
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
                let params = &req["params"];
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
                    "new_session.menu.get" | "new_session.catalog" => json!({
                        "sections": { "common": [], "terminal": [], "conversation": [] }
                    }),
                    "launch.get" => json!({ "entries": [] }),
                    "project.watch" => json!({}),
                    "git.discover" => json!({
                        "repos": [{
                            "root": "/work/smelt",
                            "rel_path": "",
                            "kind": "root"
                        }],
                        "truncated": false
                    }),
                    "git.status" => {
                        let files: Vec<Value> = (0..40)
                            .map(|i| json!([" M", format!("crates/smelt-nsh/src/mod_{i:02}.rs")]))
                            .collect();
                        json!({
                            "ok": true,
                            "branch": "js-plugins",
                            "upstream": "origin/js-plugins",
                            "ahead": 1,
                            "behind": 0,
                            "files": files,
                            "worktree_hashes": {},
                            "stash_count": 0,
                            "insertions": 1200,
                            "deletions": 800,
                        })
                    }
                    "git.branches" => json!({
                        "local": ["js-plugins", "main"],
                        "remote": ["origin/js-plugins", "origin/main"]
                    }),
                    "git.diff.all" => {
                        counters.git_diff_all_served.store(true, Ordering::Release);
                        json!({
                            "text": generate_diff_all(40),
                        })
                    }
                    "git.diff.file" => {
                        let path = params["path"]
                            .as_str()
                            .unwrap_or("crates/smelt-nsh/src/mod_00.rs");
                        json!({
                            "text": generate_diff_file(path, 0),
                            "has_staged": false,
                            "is_gitlink": false,
                        })
                    }
                    "fs.read_dir" => {
                        counters.project_tree_served.store(true, Ordering::Release);
                        let dir = params["path"].as_str().unwrap_or("/work/smelt");
                        json!({ "entries": generate_project_tree(dir) })
                    }
                    "file.open" => {
                        counters.file_open_served.store(true, Ordering::Release);
                        json!({
                            "content": {
                                "kind": "text",
                                "text": generate_large_rust_file(800),
                                "version": "v1",
                            }
                        })
                    }
                    _ => json!({}),
                };
                let resp = serde_json::to_value(ControlResponse::ok(request_id, payload))
                    .expect("serialize control response");
                let _ = writeln!(&server, "{resp}");
            });
            Ok(client)
        }))
    }

    fn connector(&self, machine: &RuntimeKey, _cx: &App) -> Option<Arc<dyn Connector>> {
        if *machine != RuntimeKey::Local {
            return None;
        }
        self.state
            .connector
            .as_ref()
            .map(|c| Arc::clone(c) as Arc<dyn Connector>)
    }

    fn machine_online(&self, machine: &RuntimeKey, _cx: &App) -> bool {
        *machine == RuntimeKey::Local
    }

    fn connections(&self, _cx: &App) -> api::Connections {
        api::Connections::default()
    }

    fn view_shown(&self, id: &str, _cx: &App) -> bool {
        self.state.shown_views.borrow().contains(id)
    }

    fn view_laid_out(&self, id: &str, _cx: &App) -> bool {
        self.state.laid_out_views.borrow().contains(id)
    }

    fn main_window(&self, _cx: &App) -> Option<AnyWindowHandle> {
        *self.state.main_window.borrow()
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

    fn session_states(&self, machine: &RuntimeKey, _cx: &App) -> Vec<DaemonSessionState> {
        if *machine != RuntimeKey::Local {
            return Vec::new();
        }
        // 会话状态按终端 id 对上对话（见 `workspace_directory` 的 `t-{i}`）。
        (0..self.state.running.min(BENCH_CONVERSATIONS))
            .map(|i| {
                serde_json::from_value(json!({
                    "id": format!("t-{i}"),
                    "phase": "thinking",
                    "runtime": true,
                    "turn_events": true,
                }))
                .expect("running session state")
            })
            .collect()
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
        let rev = self.state.dir_revision.load(Ordering::Relaxed);
        let conversations = (0..BENCH_CONVERSATIONS)
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

fn generate_diff_file(path: &str, seed: usize) -> String {
    let mut out = String::with_capacity(4096);
    out.push_str(&format!(
        "diff --git a/{path} b/{path}\nindex 1111111..2222222 100644\n--- a/{path}\n+++ b/{path}\n"
    ));
    for hunk in 0..2 {
        let start = 10 + hunk * 35;
        out.push_str(&format!(
            "@@ -{start},16 +{start},20 @@ fn module_step_{seed}_{hunk}()\n"
        ));
        for line in 0..8 {
            out.push_str(&format!(
                "     let frame_{line} = collector.sample({seed}, {line});\n"
            ));
        }
        for line in 0..4 {
            out.push_str(&format!(
                "-    let legacy_cost_{line} = old_budget(frame_{line});\n"
            ));
        }
        for line in 0..8 {
            out.push_str(&format!(
                "+    let phase_cost_{line} = fast_budget(frame_{line}, {seed});\n"
            ));
        }
        for line in 0..4 {
            out.push_str(&format!("     emit_sample(phase_cost_{line});\n"));
        }
    }
    out
}

fn generate_diff_all(file_count: usize) -> String {
    let mut out = String::with_capacity(file_count * 4096);
    for i in 0..file_count {
        let path = format!("crates/smelt-nsh/src/mod_{i:02}.rs");
        out.push_str(&generate_diff_file(&path, i));
    }
    out
}

fn generate_project_tree(dir: &str) -> Vec<Value> {
    if dir == "/work/smelt" {
        let mut entries = Vec::with_capacity(120);
        for i in 0..20 {
            entries.push(json!({ "name": format!("module_{i:02}"), "kind": "dir" }));
        }
        entries.push(json!({ "name": "src", "kind": "dir" }));
        for i in 0..99 {
            let ext = if i % 3 == 0 { "tsx" } else { "rs" };
            entries.push(json!({ "name": format!("bench_file_{i:02}.{ext}"), "kind": "file" }));
        }
        entries
    } else {
        (0..30)
            .map(|i| json!({ "name": format!("sub_entry_{i:02}.rs"), "kind": if i < 5 { "dir" } else { "file" } }))
            .collect()
    }
}

fn generate_large_rust_file(lines: usize) -> String {
    let mut out = String::with_capacity(lines * 64);
    out.push_str("//! 合成大型 Rust 源码文件，用于压测文件编辑器语法高亮与滚动。\n\n");
    out.push_str("use std::collections::BTreeMap;\n\n");
    out.push_str("pub struct MetricsBuffer {\n    entries: BTreeMap<usize, u64>,\n}\n\n");
    let mut emitted = 7usize;
    let mut idx = 0usize;
    while emitted < lines {
        out.push_str(&format!(
            "pub fn compute_frame_stage_{idx:03}(input: u64, scale: usize) -> u64 {{\n\
             \x20   let base = input.saturating_mul(scale as u64 + 1);\n\
             \x20   let adjusted = (base ^ 0x55aa_55aa).rotate_left({} );\n\
             \x20   adjusted.wrapping_add({idx})\n\
             }}\n\n",
            (idx % 31) + 1
        ));
        emitted += 6;
        idx += 1;
    }
    out
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

pub struct RealShellSetup {
    pub shell: Entity<MainShell>,
    pub host_state: BenchHostState,
    pub stage_ms: Option<f64>,
    pub boot_started: Instant,
    pub temp_dir: Option<PathBuf>,
    pub _subscriptions: Vec<Subscription>,
}

/// 模拟的工作区里有几段对话。
const BENCH_CONVERSATIONS: usize = 30;

/// 整窗场景的可选项（命令行 `--running`、`--user-plugins`）。
pub struct ShellOptions {
    /// 前几个模拟会话处于运行中。
    pub running: usize,
    /// 当用户插件目录一起加载，同 id 覆写自带插件。
    pub user_plugins: Option<PathBuf>,
}

pub fn setup_real_shell(
    staged: bool,
    connector: Option<Arc<ArcTerminalConnector>>,
    options: ShellOptions,
    window: &mut Window,
    cx: &mut Context<impl Render>,
) -> RealShellSetup {
    let repo_plugins = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
    let (plugins_dir, stage_ms, temp_dir) = if staged {
        let staged_dir =
            std::env::temp_dir().join(format!("smelt-nsh-bench-staged-{}", uuid::Uuid::new_v4()));
        let report =
            smelt_nsh::stage_builtin_plugins(&repo_plugins, &staged_dir).expect("预编译内置插件");
        eprintln!(
            "BOOT_STAGE: {}ms (total={}, script={}, module={}, theme_only={}, layout_only={})",
            report.elapsed_ms,
            report.total,
            report.script_plugins,
            report.module_plugins,
            report.theme_only,
            report.layout_only
        );
        (
            staged_dir.clone(),
            Some(report.elapsed_ms as f64),
            Some(staged_dir),
        )
    } else {
        (repo_plugins, None, None)
    };

    // 从预编译阶段之后起算冷启动挂载耗时，保持与原 real-boot-staged 口径一致。
    let boot_started = Instant::now();
    let mut host_state = BenchHostState::new(connector, Some(window.window_handle()));
    host_state.running = options.running;
    smelt_ui::init(cx);
    api::set_locale("zh-CN", cx);
    api::install(BenchHost::new(host_state.clone()), cx);
    api::register_views(
        "smelt",
        vec![stub_spec("outlet"), stub_spec("settings")],
        cx,
    )
    .expect("register smelt stub views");

    let shell_slot: Rc<RefCell<Option<Entity<MainShell>>>> = Rc::new(RefCell::new(None));
    let outlet_seq = Rc::new(Cell::new(1u64));
    register_bench_app_commands(&shell_slot, &outlet_seq, &host_state, cx);

    let shell = cx.new(|cx| {
        MainShell::new(
            ShellConfig {
                sources: std::iter::once(SourceDir {
                    path: plugins_dir,
                    source: PluginSource::Builtin,
                    watch: false,
                })
                .chain(options.user_plugins.map(|path| SourceDir {
                    path,
                    source: PluginSource::User,
                    watch: false,
                }))
                .collect(),
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
    *shell_slot.borrow_mut() = Some(shell.clone());

    let obs_state = host_state.clone();
    let sub = cx.subscribe(&shell, move |_, shell_entity, ev: &MainShellEvent, cx| {
        if matches!(
            ev,
            MainShellEvent::StateChanged | MainShellEvent::LayoutChanged { .. }
        ) {
            let view = shell_entity.read(cx).shell_view().clone();
            sync_shell_visibility(view, &obs_state, cx);
        }
    });

    RealShellSetup {
        shell,
        host_state,
        stage_ms,
        boot_started,
        temp_dir,
        _subscriptions: vec![sub],
    }
}

fn register_bench_app_commands(
    shell_slot: &Rc<RefCell<Option<Entity<MainShell>>>>,
    outlet_seq: &Rc<Cell<u64>>,
    host_state: &BenchHostState,
    cx: &mut App,
) {
    let open_shell = Rc::clone(shell_slot);
    let open_seq = Rc::clone(outlet_seq);
    let open_state = host_state.clone();
    let specs = vec![
        api::SHOW_MAIN_WINDOW.spec("Show main window", |(), _, _| Ok(())),
        api::SET_VIEW_BADGE.spec("Set view badge", |args, _, cx| {
            api::set_view_badge(&args.view, args.label.map(Into::into), cx);
            Ok(())
        }),
        api::NOTIFY.spec("Notify", |_, _, _| Ok(())),
        api::OPEN_VIEW.spec("Open view", move |args, window, cx| {
            let Some(window) = window else {
                return Ok(false);
            };
            let Some(shell) = open_shell.borrow().clone() else {
                return Ok(false);
            };
            let Some((view, _)) = api::ensure_view(&args.view, window, cx) else {
                return Ok(false);
            };
            let shown_in_layout = shell.update(cx, |s, cx| {
                if s.show_view("smelt.layouts/workbench", &args.view, cx) {
                    return true;
                }
                let next_seq = open_seq.get().wrapping_add(1);
                open_seq.set(next_seq);
                s.set_outlet(
                    "smelt.layouts/workbench",
                    Some(OutletTarget {
                        id: args.view.clone(),
                        args: args.args.clone(),
                        seq: next_seq,
                        managed_rust_instance: false,
                    }),
                    cx,
                );
                false
            });
            let _ = shown_in_layout;
            let view_snapshot = shell.read(cx).shell_view().clone();
            sync_shell_visibility(view_snapshot, &open_state, cx);
            view.open(&args.args, window, cx);
            Ok(true)
        }),
    ];
    api::register_commands("smelt.app", "Smelt", specs, cx)
        .expect("register smelt.app bench commands");
}
