//! NSH A2 主壳端到端集成测试：
//! 从仓库 `plugins/` 真实装载全部内置 NSH 插件，在真实 GPUI 窗口中验证编译、首批就绪信号、主题/页面/布局/
//! 视图/命令登记、无损存档往返、毛玻璃材质区域回报、热重载与关窗后台常驻。

use std::{
    cell::RefCell,
    collections::BTreeMap,
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
    AnyWindowHandle, App, AppContext as _, Bounds, Context, IntoElement, Pixels, Render,
    TestAppContext, Window, div, px, size,
};
use serde_json::{Value, json};
use smelt_nsh::{
    BackdropHooks, BackdropPlacement, MainShell, OutletTarget, PluginSource, ShellConfig,
    ShellLabels, SourceDir, WindowCorners,
};
use smelt_plugin_api::{
    self as api, Host, HostEvent, Machine, NotificationPermission, PluginView, RuntimeKey,
    ViewHandle, ViewSpec,
    client::{Connector, RuntimeClient, RuntimeStream, protocol::control::ControlResponse},
    model::{
        daemon_state::{DaemonSessionState, RuntimePresence},
        runtime_path::PathStyle,
        workspace_directory::WorkspaceDirectory,
    },
};

struct TestHost {
    local_online: Arc<AtomicBool>,
    rpc_methods: Arc<Mutex<Vec<String>>>,
}

impl TestHost {
    fn new(local_online: Arc<AtomicBool>, rpc_methods: Arc<Mutex<Vec<String>>>) -> Self {
        Self {
            local_online,
            rpc_methods,
        }
    }
}

impl Host for TestHost {
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
        let methods = self.rpc_methods.clone();
        Some(RuntimeClient::new(move || {
            let (client, server) = RuntimeStream::pair()?;
            let methods = methods.clone();
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
                methods.lock().unwrap().push(method.to_owned());
                let payload = match method {
                    "quota.get" | "quota.refresh" => json!({
                        "targets": ["claude"],
                        "statuses": [[
                            "claude",
                            {
                                "quota": {
                                    "windows": {
                                        "windows": [
                                            {
                                                "label": "5h",
                                                "used_percent": 42.0,
                                                "resets_at_ms": null,
                                                "reset_after_seconds": 3600,
                                                "scope": null
                                            }
                                        ]
                                    }
                                },
                                "checked_at_ms": 1_700_000_000_000i64,
                                "error": null
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
        serde_json::from_value(json!({
            "epoch": "1",
            "revision": 1,
            "projects": ["/work/smelt"],
            "worksets": [],
            "repositories": [],
            "conversations": [{
                "id": "conv-1",
                "acp": null,
                "terminals": [],
                "archived_at": null,
                "last_updated_at": 1_700_000_000
            }]
        }))
        .ok()
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

#[gpui::test]
fn main_shell_boots_all_builtin_plugins_with_standard_layout(cx: &mut TestAppContext) {
    struct TestLogger;
    impl log::Log for TestLogger {
        fn enabled(&self, meta: &log::Metadata<'_>) -> bool {
            meta.level() <= log::Level::Warn
        }
        fn log(&self, record: &log::Record<'_>) {
            if self.enabled(record.metadata()) {
                eprintln!("[{}:{}] {}", record.target(), record.level(), record.args());
            }
        }
        fn flush(&self) {}
    }
    static LOGGER: TestLogger = TestLogger;
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(log::LevelFilter::Warn);

    let local_online = Arc::new(AtomicBool::new(false));
    let rpc_methods = Arc::new(Mutex::new(Vec::<String>::new()));

    cx.executor().allow_parking();
    cx.update(|cx| {
        smelt_ui::init(cx);
        api::set_locale("en", cx);
        api::install(TestHost::new(local_online.clone(), rpc_methods.clone()), cx);
        api::register_views(
            "smelt",
            vec![stub_spec("outlet"), stub_spec("settings")],
            cx,
        )
        .unwrap();
    });

    let repo_plugins = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins");

    // 校验仓库 plugins/ 下每一个内置插件目录都能通过 headless check_plugin。
    for entry in std::fs::read_dir(&repo_plugins).expect("读取 plugins/") {
        let path = entry.expect("dir entry").path();
        if path.join(smelt_nsh::MANIFEST_FILE).is_file() {
            smelt_nsh::check_plugin(&path).unwrap_or_else(|err| {
                panic!("内置插件 {} check_plugin 失败：{err}", path.display())
            });
        }
    }

    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let user_plugins = TempDir(std::env::temp_dir().join(format!(
        "smelt-nsh-main-shell-user-{}",
        uuid::Uuid::new_v4()
    )));
    let i18n_plugin_dir = user_plugins.0.join("i18n-reload");
    std::fs::create_dir_all(i18n_plugin_dir.join("locales")).unwrap();
    std::fs::write(
        i18n_plugin_dir.join("smelt-plugin.json"),
        json!({
            "id": "test.i18n-reload",
            "name": "I18n Reload",
            "version": "1.0.0",
            "entry": "nsh.tsx"
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        i18n_plugin_dir.join("locales/ui.yml"),
        "_version: 2\nview.title:\n  en: \"Hello V1\"\n  zh-CN: \"你好 V1\"\n",
    )
    .unwrap();
    let i18n_plugin_source = |version: u32| {
        format!(
            r#"import {{ Text }} from "nsh";
import {{ bootTask, initHost, onCommand, registerCommands, registerViews, t, text }} from "smelt/sdk";
export default function main() {{
  return <Text>{{t("view.title")}}</Text>;
}}
onCommand(({{ name }}) => {{
  if (name === "check") return {{ version: {version}, label: t("view.title") }};
  return null;
}});
bootTask(async () => {{
  await initHost();
  await registerViews([{{ name: "main", export: "default", title: text("view.title"), icon: "icons/book-open.svg" }}]);
  await registerCommands([{{ name: "check", title: text("view.title") }}]);
}});
"#
        )
    };
    std::fs::write(i18n_plugin_dir.join("nsh.tsx"), i18n_plugin_source(1)).unwrap();

    let bg_plugin_dir = user_plugins.0.join("bg-topic-menu");
    std::fs::create_dir_all(&bg_plugin_dir).unwrap();
    std::fs::write(
        bg_plugin_dir.join("smelt-plugin.json"),
        json!({
            "id": "test.bg-topic-menu",
            "name": "Background Topic Menu",
            "version": "1.0.0",
            "entry": "nsh.tsx"
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        bg_plugin_dir.join("nsh.tsx"),
        r#"import { bootTask, initHost, onEvent, registerTopics, setStatusMenu } from "smelt/sdk";
onEvent((event) => {
  if (event.type !== "runtime_topic" || event.topic !== "attention.changed") return;
  const sessions = event.message?.kind === "snapshot" ? (event.message?.payload?.sessions ?? []) : [];
  const count = sessions.length;
  void setStatusMenu(count > 0 ? {
    title: `${count}@`,
    tooltip: "attention",
    items: [{ type: "action", label: "Open", command: "smelt.app/show-main-window", args: null }],
  } : null);
}, ["runtime_topic"]);
bootTask(async () => {
  await initHost();
  await registerTopics(["attention.changed"]);
});
"#,
    )
    .unwrap();

    let recorded_regions: Rc<RefCell<BTreeMap<&'static str, Bounds<Pixels>>>> =
        Rc::new(RefCell::new(BTreeMap::new()));
    let reset_count = Rc::new(RefCell::new(0usize));

    let hooks = BackdropHooks {
        reset: Some({
            let recorded = recorded_regions.clone();
            let count = reset_count.clone();
            Rc::new(move || {
                *count.borrow_mut() += 1;
                recorded.borrow_mut().clear();
            })
        }),
        record: Some({
            let recorded = recorded_regions.clone();
            Rc::new(move |placement, bounds, _| {
                let key = match placement {
                    BackdropPlacement::Center => "center",
                    BackdropPlacement::Left => "left",
                    BackdropPlacement::Right => "right",
                    BackdropPlacement::Bottom => "bottom",
                };
                match bounds {
                    Some(bounds) => {
                        recorded.borrow_mut().insert(key, bounds);
                    }
                    None => {
                        recorded.borrow_mut().remove(key);
                    }
                }
            })
        }),
    };

    let (shell, cx) = cx.add_window_view(|window, cx| {
        MainShell::new(
            ShellConfig {
                sources: vec![
                    SourceDir {
                        path: repo_plugins,
                        source: PluginSource::Builtin,
                        watch: false,
                    },
                    SourceDir {
                        path: user_plugins.0.clone(),
                        source: PluginSource::User,
                        watch: true,
                    },
                ],
                backdrop: hooks,
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

    let expected_views = [
        "smelt.agents/main",
        "smelt.agents/settings",
        "smelt.archive/button",
        "smelt.automations/main",
        "smelt.capabilities/main",
        "smelt.capabilities/memory",
        "smelt.capabilities/models",
        "smelt.desktop/desktop",
        "smelt.files/main",
        "smelt.git/main",
        "smelt.history/main",
        "smelt.launcher/nav",
        "smelt.launcher/settings",
        "smelt.machines/remote",
        "smelt.quota/bar",
        "smelt.quota/settings",
        "smelt.resources/main",
        "smelt.session-list/main",
        "smelt.session-list/settings",
        "smelt.sessions/session",
        "smelt.skills/main",
    ];
    let expected_commands = [
        "smelt.agents/agent-settings",
        "smelt.agents/edit-agent-settings",
        "smelt.agents/new-conversation",
        "smelt.capabilities/open-on-machine",
        "smelt.desktop/open",
        "smelt.files/open",
        "smelt.files/save",
        "smelt.git/next-hunk",
        "smelt.git/open-pull-request",
        "smelt.git/prev-hunk",
        "smelt.ide/open-with",
        "smelt.machines/add-machine",
        "smelt.machines/remove",
        "smelt.session-list/reveal-in-list",
        "smelt.session-list/new-session",
        "smelt.session-list/open-project",
        "smelt.session-list/focus-list",
        "smelt.session-list/prev",
        "smelt.session-list/next",
        "smelt.sessions/activate",
        "smelt.sessions/close-pane",
        "smelt.sessions/copy-session-id",
        "smelt.sessions/create-conversation",
        "smelt.sessions/fork",
        "smelt.sessions/handoff",
        "smelt.sessions/next-pane",
        "smelt.sessions/prev-pane",
        "smelt.sessions/resume",
        "smelt.sessions/resume-in-terminal",
        "smelt.sessions/select-project",
        "smelt.sessions/selection",
        "smelt.sessions/send-text",
        "smelt.sessions/split-down",
        "smelt.sessions/split-right",
        "smelt.sessions/start-conversation",
    ];

    let start = Instant::now();
    loop {
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        if shell.read_with(cx, |s, _| s.initial_plugins_settled()) {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "未等到全部内置 NSH 插件完成登记（settled=false）；已发现插件：{:?}；已登记视图：{:?}；已登记命令：{:?}；首页：{:?}",
            shell.read_with(cx, |s, _| s.installed_plugins()),
            cx.update(|_, cx| api::views(cx).into_iter().map(|v| v.id).collect::<Vec<_>>()),
            cx.update(|_, cx| api::commands(cx)
                .into_iter()
                .map(|c| c.id)
                .collect::<Vec<_>>()),
            cx.update(|_, cx| api::home_page(cx).map(|p| p.id)),
        );
        // 轮询例外：测试平台没有「引擎线程完成插件加载并登记」的单一事件；周期 20ms，上限 30s。
        std::thread::sleep(Duration::from_millis(20));
    }
    cx.run_until_parked();
    cx.update(|_, cx| {
        let missing_views: Vec<_> = expected_views
            .iter()
            .copied()
            .filter(|id| !api::is_registered(id, cx))
            .collect();
        assert!(
            missing_views.is_empty(),
            "initial_plugins_settled 时仍有视图未登记（bootTask 竞态）：{missing_views:?}"
        );
        let missing_commands: Vec<_> = expected_commands
            .iter()
            .copied()
            .filter(|id| api::command(id, cx).is_none())
            .collect();
        assert!(
            missing_commands.is_empty(),
            "initial_plugins_settled 时仍有命令未登记（bootTask 竞态）：{missing_commands:?}"
        );
        assert_eq!(
            api::home_page(cx).map(|p| p.id.to_string()).as_deref(),
            Some("smelt.layouts/workbench"),
            "initial_plugins_settled 时首页尚未登记"
        );
        assert!(
            api::layout("smelt.layouts/standard", cx).is_some(),
            "initial_plugins_settled 时标准布局尚未登记"
        );
        assert!(
            api::theme_contains("Ayu Dark", cx),
            "initial_plugins_settled 时 Ayu Dark 主题尚未登记"
        );
    });

    // 所有 20 个内置插件（含共享模块 smelt.conversation、纯主题 smelt.themes、纯布局 smelt.layouts）都无编译/运行错误。
    let plugins = shell.read_with(cx, |s, _| s.installed_plugins());
    let builtin_count = plugins
        .iter()
        .filter(|p| p.source == PluginSource::Builtin)
        .count();
    assert_eq!(builtin_count, 20, "应发现 20 个内置插件：{plugins:?}");
    for p in &plugins {
        assert!(p.error.is_none(), "插件 {} 报错：{:?}", p.id, p.error);
    }
    let conv = plugins
        .iter()
        .find(|p| p.id == "smelt.conversation")
        .expect("应发现内置共享模块插件 smelt.conversation");
    assert_eq!(conv.module.as_deref(), Some("smelt/conversation"));
    let sessions = plugins
        .iter()
        .find(|p| p.id == "smelt.sessions")
        .expect("应发现内置插件 smelt.sessions");
    assert_eq!(sessions.used_modules, vec!["smelt.conversation"]);

    let mut vm_ids = shell.read_with(cx, |s, _| s.plugin_vm_ids());
    vm_ids.sort();
    assert_eq!(
        vm_ids,
        vec!["__builtins__", "test.bg-topic-menu", "test.i18n-reload"],
        "全部 16 个内置脚本插件应共享 1 个 __builtins__ VM，2 个用户插件各自独立 VM：{vm_ids:?}"
    );

    // 验证 smelt.ide 登记的项目右键菜单仅本机可见、远程不可见（验证 machine: null 解析为 Some(Local)）。
    cx.update(|_, cx| {
        let local_items =
            api::context_menu_items("smelt.session-list/project", &RuntimeKey::Local, cx);
        assert!(
            !local_items.is_empty(),
            "本机项目菜单应含 smelt.ide 的用其他应用打开子菜单"
        );
        let remote_items = api::context_menu_items(
            "smelt.session-list/project",
            &RuntimeKey::Remote("far".into()),
            cx,
        );
        assert!(
            remote_items.is_empty(),
            "远程项目菜单不应含 smelt.ide 的本机应用菜单：{remote_items:?}"
        );
    });

    // 直接使用 plugins/layouts 插件登记到注册表的出厂布局。
    let standard_state = cx
        .update(|_, cx| api::layout("smelt.layouts/standard", cx))
        .expect("plugins/layouts 应登记 smelt.layouts/standard");

    // 装入真实出厂布局 standard.json，并将出口指向 smelt.sessions/session。
    shell.update(cx, |shell, cx| {
        shell
            .load_page("smelt.layouts/workbench".into(), &standard_state, cx)
            .expect("装入 standard.json 布局");
        shell.set_active_page(
            "smelt.layouts/workbench".into(),
            Some(OutletTarget {
                id: "smelt.sessions/session".into(),
                args: Value::Null,
                seq: 1,
                managed_rust_instance: false,
            }),
            cx,
        );
    });

    // 验证快照可见视图与无损存档往返。
    let (shown, dumped) = shell.read_with(cx, |s, _| {
        (
            s.shell_view().shown.clone(),
            s.dump_page("smelt.layouts/workbench").expect("dump_page"),
        )
    });
    for id in [
        "smelt.launcher/nav",
        "smelt.session-list/main",
        "smelt.launcher/settings",
        "smelt.archive/button",
        "smelt.quota/bar",
        "smelt/outlet",
        "smelt.sessions/session",
    ] {
        assert!(
            shown.contains(id),
            "standard.json 中 {id} 应为可见，实际：{shown:?}"
        );
    }
    let stacks = api::layout_stacks(&dumped);
    assert_eq!(stacks.len(), 1, "dump_page 应无损保留左侧栏 smelt/stack");
    assert_eq!(
        stacks[0].views(),
        [
            "smelt.launcher/nav",
            "smelt.session-list/main",
            "smelt.launcher/settings",
            "smelt.archive/button",
            "smelt.quota/bar"
        ]
    );

    // 等 NSH 渲染出 DockArea 并经 prepaint 回报左右/中间材质区域坐标。
    let start = Instant::now();
    loop {
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let has_regions = {
            let r = recorded_regions.borrow();
            r.get("left").is_some_and(|b| b.size.width > px(100.))
                && r.get("center").is_some_and(|b| b.size.width > px(200.))
        };
        if has_regions {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "未收到 DockArea 材质区域回报（resets={}，regions={:?}）",
            *reset_count.borrow(),
            *recorded_regions.borrow(),
        );
        // 轮询例外：测试平台等待引擎线程提交布局帧；周期 20ms，上限 15s。
        std::thread::sleep(Duration::from_millis(20));
    }

    // 回归测试：切换会话更新出口参数与状态推送时，DockArea 绝不能整棵卸载重建（每帧材质区域持续存在）。
    for seq in 2..=4 {
        shell.update(cx, |shell, cx| {
            shell.set_outlet(
                "smelt.layouts/workbench",
                Some(OutletTarget {
                    id: "smelt.sessions/session".into(),
                    args: json!({ "id": format!("conv-{seq}") }),
                    seq,
                    managed_rust_instance: false,
                }),
                cx,
            );
        });
        for _ in 0..4 {
            // 轮询例外：等待引擎线程处理状态推送并提交帧，验证连续多帧均不掉块；周期 15ms，上限 4 次。
            std::thread::sleep(Duration::from_millis(15));
            cx.run_until_parked();
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
            let r = recorded_regions.borrow();
            assert!(
                r.get("left").is_some_and(|b| b.size.width > px(100.))
                    && r.get("center").is_some_and(|b| b.size.width > px(200.)),
                "切换会话 seq={seq} 时 DockArea 出现卸载闪烁（regions={r:?}）"
            );
        }
    }

    // 验证双向命令派发：宿主执行 JS 插件命令 `smelt.session-list/new-session`。
    let cmd_result: Rc<RefCell<Option<Result<Value, String>>>> = Rc::new(RefCell::new(None));
    let cmd_slot = cmd_result.clone();
    cx.update(|_, cx| {
        let task = api::run("smelt.session-list/new-session", json!(null), cx);
        cx.spawn(async move |_| {
            let res = task.await;
            *cmd_slot.borrow_mut() = Some(res);
        })
        .detach();
    });
    let start = Instant::now();
    loop {
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        if cmd_result.borrow().is_some() {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "调用 JS 插件命令 smelt.session-list/new-session 超时"
        );
        // 轮询例外：测试平台等待引擎线程通过 command.reply 返回结果；周期 20ms，上限 10s。
        std::thread::sleep(Duration::from_millis(20));
    }
    let result = cmd_result.borrow_mut().take().unwrap();
    assert!(
        result.is_ok(),
        "调用 JS 插件命令 smelt.session-list/new-session 失败：{result:?}（耗时 {:?}）",
        start.elapsed()
    );

    // 回归测试：通过视图代理打开 smelt.sessions/session 切会话（曾因漏 import locateSession 静默报错）。
    cx.update(|window, cx| {
        let (view, _) = api::ensure_view("smelt.sessions/session", window, cx)
            .expect("smelt.sessions/session 已登记");
        view.open(
            &json!({ "machine": null, "conversation_id": "conv-1" }),
            window,
            cx,
        );
    });
    let start = Instant::now();
    loop {
        let sel_result: Rc<RefCell<Option<Result<Value, String>>>> = Rc::new(RefCell::new(None));
        let sel_slot = sel_result.clone();
        cx.update(|_, cx| {
            let task = api::run("smelt.sessions/selection", json!(null), cx);
            cx.spawn(async move |_| {
                *sel_slot.borrow_mut() = Some(task.await);
            })
            .detach();
        });
        let call_start = Instant::now();
        while sel_result.borrow().is_none() {
            cx.run_until_parked();
            cx.update(|window, _| window.refresh());
            assert!(
                call_start.elapsed() < Duration::from_secs(5),
                "查询 smelt.sessions/selection 超时"
            );
            // 轮询例外：测试平台等待引擎线程返回命令结果；周期 20ms，上限 5s。
            std::thread::sleep(Duration::from_millis(20));
        }
        let got = sel_result
            .borrow_mut()
            .take()
            .unwrap()
            .expect("smelt.sessions/selection 不应失败");
        if got["kind"] == "conversation" && got["conversation_id"] == "conv-1" {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "打开 smelt.sessions/session 后未切到 conv-1，当前 selection={got}"
        );
        // 轮询例外：测试平台等待 view_opened 事件在会话插件 VM 内生效；周期 20ms，上限 5s。
        std::thread::sleep(Duration::from_millis(20));
    }

    // 验证事件驱动 waitOnline：本机离线时不查额度；本机上线并发 MachinesChanged 后立即拉取 quota.get 与 quota.refresh。
    assert!(
        !rpc_methods.lock().unwrap().iter().any(|m| m == "quota.get"),
        "本机离线时不应提前发 quota.get"
    );
    local_online.store(true, Ordering::SeqCst);
    cx.update(|_, cx| api::emit(HostEvent::MachinesChanged, cx));
    let start = Instant::now();
    loop {
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let has_quota = {
            let calls = rpc_methods.lock().unwrap();
            calls.iter().any(|m| m == "quota.get") && calls.iter().any(|m| m == "quota.refresh")
        };
        if has_quota {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "本机上线后未触发 quota.get / quota.refresh，实际调用：{:?}",
            *rpc_methods.lock().unwrap(),
        );
        // 轮询例外：测试平台等待插件 VM 经 wait_online 唤醒并完成 RPC；周期 20ms，上限 10s。
        std::thread::sleep(Duration::from_millis(20));
    }

    // 切到设置页里的全部 JS 设置视图，确认渲染无致命错误，且额度设置页发起了 quota.settings.get。
    for (id, title) in [
        ("smelt.agents/settings", "Agents"),
        ("smelt.capabilities/models", "Models"),
        ("smelt.capabilities/memory", "Memory"),
        ("smelt.machines/remote", "Remote"),
        ("smelt.quota/settings", "Quota"),
        ("smelt.resources/main", "Resource Usage"),
        ("smelt.session-list/settings", "Session List"),
    ] {
        shell.update(cx, |shell, cx| {
            shell.set_settings_page(Some((id.into(), title.into())), cx);
        });
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
    }
    let start = Instant::now();
    loop {
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        if rpc_methods
            .lock()
            .unwrap()
            .iter()
            .any(|m| m == "quota.settings.get")
        {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "打开 smelt.quota/settings 后未触发 quota.settings.get，实际调用：{:?}",
            *rpc_methods.lock().unwrap(),
        );
        // 轮询例外：测试平台等待设置页拉取开关；周期 20ms，上限 10s。
        std::thread::sleep(Duration::from_millis(20));
    }

    // 回归测试：热重载插件后不应把 locales 文案清空回退成原始 key。
    let run_check = |cx: &mut gpui::VisualTestContext| -> Result<Value, String> {
        let slot: Rc<RefCell<Option<Result<Value, String>>>> = Rc::new(RefCell::new(None));
        let out = slot.clone();
        cx.update(|_, cx| {
            let task = api::run("test.i18n-reload/check", json!(null), cx);
            cx.spawn(async move |_| {
                *out.borrow_mut() = Some(task.await);
            })
            .detach();
        });
        let start = Instant::now();
        while slot.borrow().is_none() {
            cx.run_until_parked();
            cx.update(|window, _| window.refresh());
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "调用 test.i18n-reload/check 超时"
            );
            // 轮询例外：测试平台等待插件 VM 返回命令结果；周期 20ms，上限 5s。
            std::thread::sleep(Duration::from_millis(20));
        }
        slot.borrow_mut().take().unwrap()
    };

    let v1 = run_check(cx).expect("首次加载 test.i18n-reload/check 应成功");
    assert_eq!(v1["version"], 1);
    assert_eq!(v1["label"], "Hello V1", "首次加载应拿到翻译后的文案");
    let v1_title = cx.update(|_, cx| {
        api::views(cx)
            .into_iter()
            .find(|v| v.id == "test.i18n-reload/main")
            .map(|v| v.title.to_string())
    });
    assert_eq!(v1_title.as_deref(), Some("Hello V1"));

    // 修改 locales 与入口脚本触发热重载，确认新 VM 拿到最新文案且不丢失。
    std::fs::write(
        i18n_plugin_dir.join("locales/ui.yml"),
        "_version: 2\nview.title:\n  en: \"Hello V2\"\n  zh-CN: \"你好 V2\"\n",
    )
    .unwrap();
    std::fs::write(i18n_plugin_dir.join("nsh.tsx"), i18n_plugin_source(2)).unwrap();

    let reload_start = Instant::now();
    let v2 = loop {
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        if let Ok(got) = run_check(cx)
            && got["version"] == 2
        {
            break got;
        }
        assert!(
            reload_start.elapsed() < Duration::from_secs(10),
            "等待 test.i18n-reload 热重载到 v2 超时"
        );
        // 轮询例外：测试平台等待 FSEvents 防抖与插件热重载完成；周期 50ms，上限 10s。
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(
        v2["label"], "Hello V2",
        "热重载后 t() 应返回最新翻译而非原始 key"
    );
    let v2_title = cx.update(|_, cx| {
        api::views(cx)
            .into_iter()
            .find(|v| v.id == "test.i18n-reload/main")
            .map(|v| v.title.to_string())
    });
    assert_eq!(
        v2_title.as_deref(),
        Some("Hello V2"),
        "热重载后视图标题应返回最新翻译而非原始 key"
    );

    // 回归测试：主窗口关闭（detach_window）后引擎和后台插件仍常驻运行：
    // 推来 attention.changed 时后台订阅插件照常更新菜单栏；重挂窗口（attach_window）后可恢复加载页面。
    shell.update(cx, |shell, cx| shell.detach_window(cx));
    cx.run_until_parked();
    cx.update(|_, cx| {
        api::emit(
            HostEvent::RuntimeTopic {
                machine: RuntimeKey::Local,
                topic: "attention.changed".into(),
                message: api::TopicMessage::Snapshot(json!({
                    "revision": 1,
                    "sessions": [{
                        "session_id": "s-detached",
                        "action": {
                            "kind": "approval",
                            "title": "Waiting for approval",
                            "message": "rm -rf target",
                            "note": null
                        },
                        "unread": {
                            "kind": "approval",
                            "title": "Waiting for approval",
                            "message": "rm -rf target",
                            "note": null
                        }
                    }]
                })),
            },
            cx,
        );
    });
    let start = Instant::now();
    loop {
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        let menu_title = cx.update(|_, cx| {
            api::status_menus(cx)
                .into_iter()
                .find(|(owner, _)| owner == "test.bg-topic-menu")
                .map(|(_, menu)| menu.title)
        });
        if menu_title.as_deref() == Some("1@") {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "detach_window 后 test.bg-topic-menu 未更新菜单栏标题为 1@，当前：{menu_title:?}"
        );
        // 轮询例外：测试平台等待引擎线程处理后台话题并回传 set_status_menu；周期 20ms，上限 10s。
        std::thread::sleep(Duration::from_millis(20));
    }
    shell.update(cx, |shell, cx| {
        shell
            .load_page("smelt.layouts/workbench".into(), &standard_state, cx)
            .unwrap();
    });

    let after = shell.read_with(cx, |s, _| s.installed_plugins());
    for p in &after {
        assert!(
            p.error.is_none(),
            "交互后插件 {} 报运行时错误：{:?}",
            p.id,
            p.error
        );
    }

    shell.update(cx, |shell, cx| shell.shutdown(cx));
}
