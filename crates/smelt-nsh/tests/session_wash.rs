//! 会话列表 WashLayer 入场/定位底色回归测试（独立进程，避开 `nsh_platform_gpui::rt::JOBS` 单进程 `OnceLock`）：
//! 模拟新建会话入场与 `reveal-in-list` 定位后，在 `WASH_HOLD_MS`（360ms）内立即收到
//! `SessionsChanged`（触发侧栏重算与 `motionTick` 递增），验证铺底必须如期淡出到完全消失，
//! 不能因为 `effect` 重跑提前清掉收尾逻辑而常驻灰底。

use std::{
    cell::RefCell,
    io::{BufRead as _, BufReader, Write as _},
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use gpui::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, IntoElement, Pixels, Render,
    TestAppContext, VisualTestContext, Window, div, px, size,
};
use serde_json::{Value, json};
use smelt_nsh::{
    BackdropHooks, MainShell, OutletTarget, PluginSource, ShellConfig, ShellLabels, SourceDir,
    WindowCorners,
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

struct WashHost {
    directory: Arc<Mutex<WorkspaceDirectory>>,
}

impl Host for WashHost {
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
                    "remote.config.get" => json!({ "enabled": false, "iroh_relay": "" }),
                    "remote.status" => json!({ "state": "off" }),
                    "device.list" => json!({ "devices": [] }),
                    "new_session.catalog" => json!({
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
        Some(self.directory.lock().unwrap().clone())
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

fn a11y_labels(cx: &mut VisualTestContext) -> Vec<(String, String)> {
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
            let role = aria.get("role")?.as_str()?.to_owned();
            let label = aria.get("label")?.as_str()?.to_owned();
            Some((role, label))
        })
        .collect()
}

fn has_session_label(cx: &mut VisualTestContext, title: &str) -> bool {
    a11y_labels(cx)
        .into_iter()
        .any(|(role, label)| role == "Button" && label.starts_with(title))
}

fn has_wash_quad(cx: &mut VisualTestContext) -> bool {
    let text_rgb = smelt_ui::ui_theme::text();
    let r_want = ((text_rgb >> 16) & 0xff) as i32;
    let g_want = ((text_rgb >> 8) & 0xff) as i32;
    let b_want = (text_rgb & 0xff) as i32;
    cx.update(|window, _| {
        let scale = window.scale_factor();
        window.painted_quads().iter().any(|quad| {
            let b = quad.bounds;
            let x = b.origin.x.0 / scale;
            let w = b.size.width.0 / scale;
            let h = b.size.height.0 / scale;
            if x >= 260. || w <= 180. || (h - 44.).abs() >= 2. {
                return false;
            }
            quad.background.as_solid().is_some_and(|color| {
                let c = gpui::Rgba::from(color);
                if c.a <= 0.005 || c.a >= 0.25 {
                    return false;
                }
                let r = (c.r * 255.0).round() as i32;
                let g = (c.g * 255.0).round() as i32;
                let bl = (c.b * 255.0).round() as i32;
                (r - r_want).abs() <= 2 && (g - g_want).abs() <= 2 && (bl - b_want).abs() <= 2
            })
        })
    })
}

fn wait_for(
    cx: &mut VisualTestContext,
    what: &str,
    timeout: Duration,
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
        if start.elapsed() >= timeout {
            panic!("未等到：{what}；当前 a11y：{:?}", a11y_labels(cx));
        }
        // 轮询例外：测试平台等待引擎/动画线程提交帧并完成重绘；周期 15ms，上限由 timeout 指定。
        std::thread::sleep(Duration::from_millis(15));
    }
}

#[gpui::test]
fn session_wash_fades_out_after_interrupting_sessions_changed(cx: &mut TestAppContext) {
    unsafe {
        std::env::set_var("GPUI_A11Y_ACTIVE", "1");
    }

    let directory = Arc::new(Mutex::new(WorkspaceDirectory {
        epoch: "1".into(),
        revision: 1,
        projects: vec!["/work/smelt".into()],
        repositories: Vec::new(),
        worksets: Vec::new(),
        conversations: vec![WorkspaceConversation {
            id: "conv-a".into(),
            custom_title: Some("Alpha Chat".into()),
            last_updated_at: 1_700_000_200,
            terminals: vec![WorkspaceTerminal {
                id: Some("t-a".into()),
                cwd: Some("/work/smelt".into()),
                ..Default::default()
            }],
            ..Default::default()
        }],
    }));

    cx.executor().allow_parking();
    cx.update(|cx| {
        smelt_ui::init(cx);
        api::set_locale("en", cx);
        api::install(
            WashHost {
                directory: Arc::clone(&directory),
            },
            cx,
        );
        api::register_views(
            "smelt",
            vec![stub_spec("outlet"), stub_spec("settings")],
            cx,
        )
        .unwrap();
        api::register_commands(
            "smelt.app",
            "App",
            vec![api::CommandSpec {
                name: "open-view".into(),
                title: "Open view".into(),
                key: None,
                context: "Workspace".into(),
                requires_args: false,
                args: None,
                cli: false,
                handler: Rc::new(move |_, _, _| {
                    api::Handled::Done(gpui::Task::ready(Ok(json!(true))))
                }),
            }],
            cx,
        )
        .unwrap();
    });

    let repo_plugins = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
    let backdrop_regions: Rc<RefCell<std::collections::BTreeMap<u8, Option<Bounds<Pixels>>>>> =
        Rc::default();
    let r_reset = backdrop_regions.clone();
    let r_record = backdrop_regions.clone();
    let hooks = BackdropHooks {
        reset: Some(Rc::new(move || {
            r_reset.borrow_mut().clear();
        })),
        record: Some(Rc::new(move |placement, bounds, _| {
            let key = match placement {
                smelt_nsh::BackdropPlacement::Center => 0u8,
                smelt_nsh::BackdropPlacement::Left => 1,
                smelt_nsh::BackdropPlacement::Right => 2,
                smelt_nsh::BackdropPlacement::Bottom => 3,
            };
            r_record.borrow_mut().insert(key, bounds);
        })),
    };

    let (shell, cx) = cx.add_window_view(|window, cx| {
        MainShell::new(
            ShellConfig {
                sources: vec![SourceDir {
                    path: repo_plugins,
                    source: PluginSource::Builtin,
                    watch: false,
                }],
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
    // 关闭离屏缓存层，避免静止后被 layer_cache 合并成纹理层而绕开 `window.painted_quads()` 校验。
    cx.update(|window, _| window.set_scroll_layers(false));

    wait_for(
        cx,
        "内置插件初始化完毕",
        Duration::from_secs(15),
        |cx| {
            shell.read_with(cx, |s, _| s.initial_plugins_settled())
                && cx.update(|_, cx| api::layout("smelt.layouts/standard", cx).is_some())
        },
    );

    let plugin_errors: Vec<String> = shell.read_with(cx, |s, _| {
        s.installed_plugins()
            .into_iter()
            .filter_map(|p| p.error.map(|e| format!("{}: {e}", p.id)))
            .collect()
    });
    assert!(
        plugin_errors.is_empty(),
        "内置插件初始化不应有错误：{plugin_errors:?}"
    );

    let app_errors: Arc<Mutex<Vec<String>>> = Arc::default();
    {
        let errs = Arc::clone(&app_errors);
        shell.read_with(cx, |s, cx| {
            s.app_view().read(cx).on_app_error(move |error| {
                errs.lock().unwrap().push(error.to_string());
            });
        });
    }

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
                id: "smelt.sessions/session".into(),
                args: Value::Null,
                seq: 1,
                managed_rust_instance: false,
            }),
            cx,
        );
    });

    wait_for(
        cx,
        "侧栏项目与 Alpha Chat 渲染完成",
        Duration::from_secs(15),
        |cx| has_session_label(cx, "Alpha Chat"),
    );
    assert!(
        !has_wash_quad(cx),
        "静息未选中、未悬停的会话行不应出现铺底 quad"
    );

    // 场景 A：新建会话入场铺底期间，360ms 内立即到达第二次 SessionsChanged（递增 motionTick）。
    cx.update(|_, cx| {
        api::emit(
            HostEvent::Plugin {
                name: "smelt.sessions/conversation-created".into(),
                payload: json!({ "machine": null, "conversation_id": "conv-fresh" }),
            },
            cx,
        );
        {
            let mut dir = directory.lock().unwrap();
            dir.revision = 2;
            dir.conversations.push(WorkspaceConversation {
                id: "conv-fresh".into(),
                custom_title: Some("Fresh Chat".into()),
                last_updated_at: 1_700_000_300,
                terminals: vec![WorkspaceTerminal {
                    id: Some("t-fresh".into()),
                    cwd: Some("/work/smelt".into()),
                    ..Default::default()
                }],
                ..Default::default()
            });
        }
        api::emit(HostEvent::SessionsChanged, cx);
    });

    wait_for(
        cx,
        "Fresh Chat 入场并出现 WashLayer 铺底 quad",
        Duration::from_secs(5),
        |cx| has_session_label(cx, "Fresh Chat") && has_wash_quad(cx),
    );

    // 在 360ms 停留期结束前立刻触发一次侧栏重算，驱动 motionTick 递增。
    cx.update(|_, cx| {
        directory.lock().unwrap().revision = 3;
        api::emit(HostEvent::SessionsChanged, cx);
    });

    wait_for(
        cx,
        "新会话入场后收到 SessionsChanged，WashLayer 铺底应在 3s 内退净",
        Duration::from_secs(3),
        |cx| !has_wash_quad(cx),
    );

    // 场景 B：reveal-in-list 触发定位铺底期间，360ms 内立即到达 SessionsChanged。
    let reveal_result: Arc<Mutex<Option<Result<Value, String>>>> = Arc::default();
    {
        let res_slot = Arc::clone(&reveal_result);
        cx.update(|_, cx| {
            let task = api::run(
                "smelt.session-list/reveal-in-list",
                json!({ "machine": null, "session_id": "t-a" }),
                cx,
            );
            cx.spawn(async move |_| {
                *res_slot.lock().unwrap() = Some(task.await);
            })
            .detach();
        });
    }

    wait_for(
        cx,
        "reveal-in-list 后出现 WashLayer 铺底 quad",
        Duration::from_secs(5),
        |cx| has_wash_quad(cx),
    );

    cx.update(|_, cx| {
        directory.lock().unwrap().revision = 4;
        api::emit(HostEvent::SessionsChanged, cx);
    });

    wait_for(
        cx,
        "reveal-in-list 定位后收到 SessionsChanged，WashLayer 铺底应在 3s 内退净",
        Duration::from_secs(3),
        |cx| !has_wash_quad(cx),
    );

    assert!(
        matches!(reveal_result.lock().unwrap().as_ref(), Some(Ok(_))),
        "reveal-in-list 应成功返回，实际：{:?}",
        *reveal_result.lock().unwrap()
    );
    assert!(
        app_errors.lock().unwrap().is_empty(),
        "插件运行时不应报任何错误：{:?}",
        *app_errors.lock().unwrap()
    );

    shell.update(cx, |shell, cx| shell.shutdown(cx));
}
