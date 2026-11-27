//! 文件面板与 Git 面板列表渲染回归测试（独立进程，避开 `nsh_platform_gpui::rt::JOBS` 单进程 `OnceLock`）：
//! 覆盖：
//! 1. 冷启动时 `fs.read_dir` 晚于首帧返回，`Loading…` 必须响应式切到文件树而不是永远卡住；
//! 2. `ContextMenu` 必须通过 `trigger={...}` 挂载文件行/目录行，而不是丢进 `children` 导致列表全空；
//! 3. 点击目录行可展开子目录、点击文件行可打开文件；
//! 4. Git 改动树（同样使用 `ContextMenu`）能渲染改动文件行；
//! 5. 打开的正文以 Blob 交给编辑器，存盘时宿主收到的就是编辑器里的正文。

use std::{
    io::{BufRead as _, BufReader, Write as _},
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use gpui::{
    AnyWindowHandle, App, AppContext as _, Context, IntoElement, Modifiers, MouseButton, Render,
    TestAppContext, VisualTestContext, Window, div, point, px, size,
};
use serde_json::{Value, json};
use smelt_nsh::{
    BackdropHooks, MainShell, OutletTarget, PluginSource, ShellConfig, ShellLabels, SourceDir,
    WindowCorners,
};
use smelt_plugin_api::{
    self as api, ActiveContext, Host, HostEvent, Machine, NotificationPermission, PluginView,
    Project, RuntimeKey, ViewHandle, ViewSpec,
    client::{Connector, RuntimeClient, RuntimeStream, protocol::control::ControlResponse},
    model::{
        daemon_state::{DaemonSessionState, RuntimePresence},
        runtime_path::PathStyle,
        workspace_directory::WorkspaceDirectory,
    },
};

struct FilesTestHost {
    local_online: Arc<AtomicBool>,
    tree_calls: Arc<Mutex<Vec<String>>>,
    opened_files: Arc<Mutex<Vec<String>>>,
    saved_texts: Arc<Mutex<Vec<Value>>>,
}

impl Host for FilesTestHost {
    fn machines(&self, _cx: &App) -> Vec<Machine> {
        vec![Machine {
            key: RuntimeKey::Local,
            name: "This Mac".into(),
            unavailable: None,
        }]
    }

    fn client(&self, machine: &RuntimeKey, _cx: &App) -> Option<RuntimeClient> {
        if *machine != RuntimeKey::Local || !self.local_online.load(Ordering::SeqCst) {
            return None;
        }
        let tree_calls = self.tree_calls.clone();
        let opened_files = self.opened_files.clone();
        let saved_texts = self.saved_texts.clone();
        Some(RuntimeClient::new(move || {
            let (client, server) = RuntimeStream::pair()?;
            let tree_calls = tree_calls.clone();
            let opened_files = opened_files.clone();
            let saved_texts = saved_texts.clone();
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
                let params = &req["params"];
                let payload = match method {
                    "fs.read_dir" => {
                        let dir = params["path"].as_str().unwrap_or("").to_owned();
                        tree_calls.lock().unwrap().push(dir.clone());
                        match dir.as_str() {
                            "/work/smelt" => json!({
                                "entries": [
                                    { "name": "crates", "kind": "dir" },
                                    { "name": "Cargo.toml", "kind": "file" }
                                ]
                            }),
                            "/work/smelt/crates" => json!({
                                "entries": [
                                    { "name": "smelt-nsh", "kind": "dir" }
                                ]
                            }),
                            _ => json!({ "entries": [] }),
                        }
                    }
                    "file.open" => {
                        let path = params["path"].as_str().unwrap_or("").to_owned();
                        opened_files.lock().unwrap().push(path);
                        json!({
                            "content": {
                                "kind": "text",
                                "text": "[workspace]\nmembers = []\n",
                                "version": "v1"
                            }
                        })
                    }
                    "file.save" => {
                        saved_texts.lock().unwrap().push(params["text"].clone());
                        json!({ "version": "v2" })
                    }
                    "git.discover" => json!({
                        "repos": [{
                            "root": "/work/smelt",
                            "rel_path": "",
                            "kind": "root"
                        }],
                        "truncated": false
                    }),
                    "git.status" => json!({
                        "ok": true,
                        "branch": "main",
                        "ahead": 0,
                        "behind": 0,
                        "upstream": "origin/main",
                        "stash_count": 0,
                        "files": [
                            [" M", "Cargo.toml"]
                        ]
                    }),
                    "git.branches" => json!({
                        "local": ["main"],
                        "remote": ["origin/main"]
                    }),
                    "git.diff" => json!({
                        "patch": "",
                        "truncated": false
                    }),
                    "remote.config.get" => json!({ "enabled": false, "iroh_relay": "" }),
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
        *machine == RuntimeKey::Local && self.local_online.load(Ordering::SeqCst)
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
        Some(WorkspaceDirectory {
            epoch: "1".into(),
            revision: 1,
            projects: vec!["/work/smelt".into()],
            ..Default::default()
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

fn a11y_texts(cx: &mut VisualTestContext) -> Vec<String> {
    let Some(raw) = cx.update(|window, _| window.debug_a11y_tree_json()) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&raw) else {
        return Vec::new();
    };
    let Some(nodes) = parsed.get("nodes").and_then(Value::as_object) else {
        return Vec::new();
    };
    nodes
        .values()
        .filter_map(|node| {
            let aria = node.get("aria")?;
            let text = aria
                .get("label")
                .and_then(Value::as_str)
                .or_else(|| aria.get("value").and_then(Value::as_str))?;
            Some(text.to_owned())
        })
        .collect()
}

fn has_a11y_text(cx: &mut VisualTestContext, text: &str) -> bool {
    a11y_texts(cx).iter().any(|t| t == text)
}

fn wait_for(
    cx: &mut VisualTestContext,
    what: &str,
    mut done: impl FnMut(&mut VisualTestContext) -> bool,
) {
    let start = Instant::now();
    loop {
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        if start.elapsed() >= Duration::from_secs(10) {
            panic!("未等到：{what}；当前 a11y：{:?}", a11y_texts(cx));
        }
        // 轮询例外：测试平台等待引擎线程提交帧并完成重绘；周期 15ms，上限 10s。
        std::thread::sleep(Duration::from_millis(15));
    }
}

#[gpui::test]
fn files_tree_renders_and_interacts_after_async_load(cx: &mut TestAppContext) {
    // 独立测试进程单测试，开启 GPUI a11y 树捕获以直接校验一等 NSH 节点可见性。
    unsafe {
        std::env::set_var("GPUI_A11Y_ACTIVE", "1");
    }

    let local_online = Arc::new(AtomicBool::new(false));
    let tree_calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let opened_files = Arc::new(Mutex::new(Vec::<String>::new()));
    let saved_texts = Arc::new(Mutex::new(Vec::<Value>::new()));

    cx.executor().allow_parking();
    cx.update(|cx| {
        smelt_ui::init(cx);
        api::set_locale("en", cx);
        api::install(
            FilesTestHost {
                local_online: local_online.clone(),
                tree_calls: tree_calls.clone(),
                opened_files: opened_files.clone(),
                saved_texts: saved_texts.clone(),
            },
            cx,
        );
        api::register_views(
            "smelt",
            vec![stub_spec("outlet"), stub_spec("settings")],
            cx,
        )
        .unwrap();
    });

    let repo_plugins = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
    for name in ["files", "git", "automations"] {
        let dir = repo_plugins.join(name);
        smelt_nsh::check_plugin(&dir)
            .unwrap_or_else(|err| panic!("插件 {name} check_plugin 失败：{err}"));
    }
    let subset_dir =
        std::env::temp_dir().join(format!("smelt-nsh-files-tree-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&subset_dir).unwrap();
    for name in ["files", "git", "layouts", "themes"] {
        #[cfg(unix)]
        std::os::unix::fs::symlink(repo_plugins.join(name), subset_dir.join(name)).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(repo_plugins.join(name), subset_dir.join(name)).unwrap();
    }
    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _guard = TempDir(subset_dir.clone());

    let (shell, cx) = cx.add_window_view(|window, cx| {
        MainShell::new(
            ShellConfig {
                sources: vec![SourceDir {
                    path: subset_dir,
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
    cx.simulate_resize(size(px(1200.), px(800.)));

    wait_for(cx, "内置插件初始化完毕", |cx| {
        shell.read_with(cx, |s, _| s.initial_plugins_settled())
            && cx.update(|_, cx| {
                api::layout("smelt.layouts/standard", cx).is_some()
                    && api::is_registered("smelt.files/main", cx)
                    && api::is_registered("smelt.git/main", cx)
            })
    });

    let standard = cx
        .update(|_, cx| api::layout("smelt.layouts/standard", cx))
        .expect("standard layout");
    shell.update(cx, |shell, cx| {
        shell
            .load_page("smelt.layouts/workbench".into(), &standard, cx)
            .unwrap();
        shell.set_active_page(
            "smelt.layouts/workbench".into(),
            Some(OutletTarget {
                id: "smelt/outlet".into(),
                args: Value::Null,
                seq: 1,
                managed_rust_instance: false,
            }),
            cx,
        );
    });
    cx.update(|_, cx| {
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

    // 1. 守护未上线时，文件面板先显示 Loading…。
    wait_for(cx, "文件面板显示 Loading…", |cx| {
        has_a11y_text(cx, "Loading…")
    });

    // 2. 守护上线重连后，fs.read_dir 返回，Loading… 必须消失，文件树行（crates、Cargo.toml）渲染出来。
    local_online.store(true, Ordering::SeqCst);
    cx.update(|_, cx| {
        api::emit(HostEvent::MachinesChanged, cx);
        api::emit(HostEvent::Reconnected(RuntimeKey::Local), cx);
    });
    wait_for(cx, "fs.read_dir 已完成抓取", |_| {
        tree_calls
            .lock()
            .unwrap()
            .iter()
            .any(|d| d == "/work/smelt")
    });
    wait_for(
        cx,
        "文件树渲染出 crates 与带 Git 状态的 Cargo.toml",
        |cx| {
            !has_a11y_text(cx, "Loading…")
                && has_a11y_text(cx, "crates")
                && has_a11y_text(cx, "Cargo.toml M")
                && has_a11y_text(cx, "All")
        },
    );

    // 3. 点击第一行目录（crates），展开并渲染子目录 smelt-nsh。
    // 窗口 1200x800，右停靠区 340px（x=860..1200），文件树固定宽 190px（x=1010..1200）；
    // 顶部标签栏 34px + 搜索栏 39px = 73px，第 1 行 y=75..99，第 2 行 y=99..123。
    let row0 = point(px(1080.), px(87.));
    cx.simulate_mouse_move(row0, None, Modifiers::default());
    cx.simulate_click(row0, Modifiers::default());
    wait_for(cx, "展开 crates 后渲染出 smelt-nsh", |cx| {
        has_a11y_text(cx, "smelt-nsh")
    });

    // 4. 展开后 Cargo.toml 下移一行到第 3 行（y=123..147），点击应触发 file.open 并渲染顶部面包屑栏。
    let row2 = point(px(1080.), px(135.));
    cx.simulate_mouse_move(row2, None, Modifiers::default());
    cx.simulate_click(row2, Modifiers::default());
    wait_for(
        cx,
        "点击 Cargo.toml 触发 file.open 并出现面包屑栏",
        |cx| {
            opened_files
                .lock()
                .unwrap()
                .iter()
                .any(|p| p == "/work/smelt/Cargo.toml")
                && has_a11y_text(cx, "Hide File Tree")
        },
    );

    // 正文以 Blob 交给编辑器：编辑器里是读到的正文；存盘时宿主收到的也是它（Blob 按文本送出）。
    wait_for(cx, "编辑器显示读到的正文", |cx| {
        a11y_texts(cx).iter().any(|t| t.contains("members = []"))
    });
    cx.update(|_, cx| api::run("smelt.files/save", json!({}), cx).detach());
    wait_for(cx, "存盘送出编辑器里的正文", |_| {
        !saved_texts.lock().unwrap().is_empty()
    });
    assert_eq!(
        saved_texts.lock().unwrap().as_slice(),
        [json!("[workspace]\nmembers = []\n")],
    );

    // 5. 打开文件后顶部多出面包屑栏（约 39px），右键文件行应弹出包含 Copy File Path 与 Delete File 的上下文菜单。
    let row2_after_breadcrumb = point(px(1080.), px(174.));
    cx.simulate_mouse_move(row2_after_breadcrumb, None, Modifiers::default());
    cx.simulate_mouse_down(
        row2_after_breadcrumb,
        MouseButton::Right,
        Modifiers::default(),
    );
    cx.simulate_mouse_up(
        row2_after_breadcrumb,
        MouseButton::Right,
        Modifiers::default(),
    );
    wait_for(cx, "右键文件行弹出上下文菜单", |cx| {
        has_a11y_text(cx, "Copy File Path") && has_a11y_text(cx, "Delete File")
    });

    shell.update(cx, |shell, cx| shell.shutdown(cx));
}
