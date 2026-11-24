//! 会话列表行内重命名交互回归测试（独立进程，避开 `nsh_platform_gpui::rt::JOBS` 单进程 `OnceLock`）：
//! 模拟真实 `Workspace` 的 `"escape" -> DismissWorkspace` 键位上下文与可聚焦右侧面板，
//! 覆盖右键「重命名」/ 双击重命名后的 Esc 取消、点其它会话行、点空白区与点右侧原生视图退出重命名。

use std::{
    cell::RefCell,
    collections::BTreeMap,
    io::{BufRead as _, BufReader, Write as _},
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use gpui::{
    AnyElement, AnyWindowHandle, App, AppContext as _, Bounds, Context, Entity, FocusHandle,
    InteractiveElement as _, IntoElement, KeyBinding, Modifiers, MouseButton, ParentElement as _,
    Pixels, Render, Styled as _, TestAppContext, VisualTestContext, Window, canvas, div, point, px,
    size,
};
use nsh_platform_gpui::NativeComponent;
use serde_json::{Value, json};
use smelt_nsh::{
    BackdropHooks, MainShell, OutletTarget, PluginSource, ShellConfig, ShellLabels, SourceDir,
    WindowCorners, testing,
};
use smelt_plugin_api::{
    self as api, Host, Machine, NotificationPermission, PluginView, RuntimeKey, ViewHandle,
    ViewSpec,
    client::{Connector, RuntimeClient, RuntimeStream, protocol::control::ControlResponse},
    model::{
        daemon_state::{DaemonSessionState, RuntimePresence},
        runtime_path::PathStyle,
        workspace_directory::{WorkspaceConversation, WorkspaceDirectory, WorkspaceTerminal},
    },
};

gpui::actions!(inline_rename_test, [DismissWorkspace]);

#[derive(Default)]
struct Seen {
    props: BTreeMap<String, Value>,
    bounds: Option<Bounds<Pixels>>,
    painted_frame: u64,
}

#[derive(Default)]
struct SpyLog {
    frame: u64,
    next: usize,
    live: BTreeMap<usize, Seen>,
}

struct SpyBuddy {
    id: usize,
    log: Rc<RefCell<SpyLog>>,
    inner: Box<dyn NativeComponent>,
}

impl NativeComponent for SpyBuddy {
    fn render(&mut self, window: &mut Window, cx: &mut App) -> Option<AnyElement> {
        let content = self.inner.render(window, cx)?;
        let id = self.id;
        let log_prepaint = self.log.clone();
        let log_paint = self.log.clone();
        let probe = canvas(
            move |bounds, _, _| {
                if let Some(seen) = log_prepaint.borrow_mut().live.get_mut(&id) {
                    seen.bounds = Some(bounds);
                }
            },
            move |bounds, (), _, _| {
                let mut log = log_paint.borrow_mut();
                let frame = log.frame;
                if let Some(seen) = log.live.get_mut(&id) {
                    seen.bounds = Some(bounds);
                    seen.painted_frame = frame;
                }
            },
        );
        Some(
            div()
                .size_full()
                .relative()
                .child(content)
                .child(probe.absolute().top_0().left_0().size_full())
                .into_any_element(),
        )
    }

    fn set_prop(&mut self, name: &str, value: Option<&Value>, window: &mut Window, cx: &mut App) {
        if let Some(seen) = self.log.borrow_mut().live.get_mut(&self.id) {
            seen.props
                .insert(name.to_owned(), value.cloned().unwrap_or(Value::Null));
        }
        self.inner.set_prop(name, value, window, cx);
    }
}

impl Drop for SpyBuddy {
    fn drop(&mut self) {
        self.log.borrow_mut().live.remove(&self.id);
    }
}

struct RenameHost {
    rename_calls: Arc<Mutex<Vec<Value>>>,
    remote_config_sets: Arc<Mutex<Vec<Value>>>,
    create_calls: Arc<Mutex<Vec<Value>>>,
}

impl Host for RenameHost {
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
        let rename_calls = self.rename_calls.clone();
        let remote_config_sets = self.remote_config_sets.clone();
        let create_calls = self.create_calls.clone();
        Some(RuntimeClient::new(move || {
            let (client, server) = RuntimeStream::pair()?;
            let rename_calls = rename_calls.clone();
            let remote_config_sets = remote_config_sets.clone();
            let create_calls = create_calls.clone();
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
                    "workspace.rename" | "workspace.workset.rename_preserving_branch" => {
                        rename_calls.lock().unwrap().push(req.clone());
                        json!({})
                    }
                    "workspace.conversation.create" => {
                        create_calls.lock().unwrap().push(req.clone());
                        json!({ "revision": 2 })
                    }
                    "remote.config.get" => json!({ "enabled": false, "iroh_relay": "" }),
                    "remote.config.set" => {
                        remote_config_sets.lock().unwrap().push(req.clone());
                        json!({ "state": "off" })
                    }
                    "remote.status" => json!({ "state": "off" }),
                    "device.list" => json!({ "devices": [] }),
                    "project.search" => json!({ "hits": [], "truncated": false }),
                    "fs.list" => json!({ "entries": [] }),
                    "new_session.catalog" => json!({
                        "sections": {
                            "common": [],
                            "terminal": [
                                {
                                    "key": "blank",
                                    "label": "Terminal",
                                    "kind": "terminal",
                                    "pinned": false,
                                    "target": "blankTerminal"
                                }
                            ],
                            "conversation": []
                        }
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
        Some(WorkspaceDirectory {
            epoch: "1".into(),
            revision: 1,
            projects: vec!["/work/smelt".into()],
            repositories: Vec::new(),
            worksets: Vec::new(),
            conversations: vec![
                WorkspaceConversation {
                    id: "conv-a".into(),
                    custom_title: Some("Alpha Chat".into()),
                    last_updated_at: 1_700_000_200,
                    terminals: vec![WorkspaceTerminal {
                        id: Some("t-a".into()),
                        cwd: Some("/work/smelt".into()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                WorkspaceConversation {
                    id: "conv-b".into(),
                    custom_title: Some("Beta Chat".into()),
                    last_updated_at: 1_700_000_100,
                    terminals: vec![WorkspaceTerminal {
                        id: Some("t-b".into()),
                        cwd: Some("/work/smelt".into()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            ],
        })
    }

    fn notification_permission(&self, _cx: &App) -> NotificationPermission {
        NotificationPermission::Unsupported
    }
}

struct FocusableNativeOutlet {
    focus: FocusHandle,
}

impl Render for FocusableNativeOutlet {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().track_focus(&self.focus).on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, window, cx| {
                window.focus(&this.focus, cx);
            }),
        )
    }
}

impl PluginView for FocusableNativeOutlet {
    fn open(&mut self, _: &Value, _: &mut Window, _: &mut Context<Self>) {}

    fn focus_target(&self, _: &App) -> Option<FocusHandle> {
        Some(self.focus.clone())
    }
}

struct WorkspaceHarness {
    focus: FocusHandle,
    shell: Entity<MainShell>,
}

impl Render for WorkspaceHarness {
    fn render(&mut self, _: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .track_focus(&self.focus)
            .key_context("Workspace")
            .on_action(|_: &DismissWorkspace, _, cx| {
                cx.propagate();
            })
            .child(self.shell.clone())
    }
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
        build: Rc::new(|_, cx| -> Rc<dyn ViewHandle> {
            Rc::new(cx.new(|cx| FocusableNativeOutlet {
                focus: cx.focus_handle(),
            }))
        }),
    }
}

fn a11y_nodes(cx: &mut VisualTestContext) -> Vec<Value> {
    let Some(raw) = cx.update(|window, _| window.debug_a11y_tree_json()) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&raw) else {
        return Vec::new();
    };
    parsed
        .get("nodes")
        .and_then(Value::as_object)
        .map(|m| m.values().cloned().collect())
        .unwrap_or_default()
}

fn text_input_by_label(cx: &mut VisualTestContext, expected: &str) -> Option<Value> {
    a11y_nodes(cx).into_iter().find(|node| {
        let role = node
            .pointer("/aria/role")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let label = node
            .pointer("/aria/label")
            .and_then(Value::as_str)
            .unwrap_or_default();
        role == "TextInput" && label == expected
    })
}

fn focused_text_input_label(cx: &mut VisualTestContext) -> Option<String> {
    let raw = cx.update(|window, _| window.debug_a11y_tree_json())?;
    let parsed: Value = serde_json::from_str(&raw).ok()?;
    let focus_id = parsed.get("gpui_focus")?.as_str()?;
    let node = parsed.get("nodes")?.get(focus_id)?;
    let role = node.pointer("/aria/role")?.as_str()?;
    if role != "TextInput" {
        return None;
    }
    node.pointer("/aria/label")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn rename_input_node(cx: &mut VisualTestContext) -> Option<Value> {
    text_input_by_label(cx, "Rename")
}

fn has_rename_input(cx: &mut VisualTestContext) -> bool {
    rename_input_node(cx).is_some()
}

fn has_a11y_label(cx: &mut VisualTestContext, label: &str) -> bool {
    a11y_nodes(cx)
        .into_iter()
        .any(|node| node.pointer("/aria/label").and_then(Value::as_str) == Some(label))
}

fn wait_until(
    cx: &mut VisualTestContext,
    timeout: Duration,
    mut done: impl FnMut(&mut VisualTestContext) -> bool,
) -> bool {
    let start = Instant::now();
    loop {
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        if done(cx) {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        // 轮询例外：测试平台等待引擎线程提交帧并完成重绘；周期 15ms，上限由调用方指定。
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn buddy_bounds(log: &Rc<RefCell<SpyLog>>, seed: &str) -> Option<Bounds<Pixels>> {
    let log = log.borrow();
    let frame = log.frame;
    log.live
        .values()
        .filter(|seen| {
            seen.painted_frame == frame
                && seen.props.get("seed").and_then(Value::as_str) == Some(seed)
        })
        .find_map(|seen| {
            seen.bounds
                .filter(|b| b.size.width > px(0.) && b.size.height > px(0.))
        })
}

#[gpui::test]
fn inline_rename_can_be_cancelled_and_dismissed(cx: &mut TestAppContext) {
    unsafe {
        std::env::set_var("GPUI_A11Y_ACTIVE", "1");
    }

    let rename_calls = Arc::new(Mutex::new(Vec::<Value>::new()));
    let remote_config_sets = Arc::new(Mutex::new(Vec::<Value>::new()));
    let open_view_calls = Arc::new(Mutex::new(Vec::<Value>::new()));
    let create_calls = Arc::new(Mutex::new(Vec::<Value>::new()));
    let spy = Rc::new(RefCell::new(SpyLog::default()));

    cx.executor().allow_parking();
    let open_view_calls_cmd = open_view_calls.clone();
    cx.update(|cx| {
        smelt_ui::init(cx);
        api::set_locale("en", cx);
        cx.bind_keys([KeyBinding::new(
            "escape",
            DismissWorkspace,
            Some("Workspace"),
        )]);
        api::install(
            RenameHost {
                rename_calls: rename_calls.clone(),
                remote_config_sets: remote_config_sets.clone(),
                create_calls: create_calls.clone(),
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
            vec![
                api::CommandSpec {
                    name: "open-view".into(),
                    title: "Open view".into(),
                    key: None,
                    context: "Workspace".into(),
                    palette: false,
                    requires_args: false,
                    args: None,
                    cli: false,
                    handler: Rc::new(move |args, _, _| {
                        open_view_calls_cmd.lock().unwrap().push(args.clone());
                        api::Handled::Done(gpui::Task::ready(Ok(json!(true))))
                    }),
                },
                api::CommandSpec {
                    name: "show-main-window".into(),
                    title: "Show main window".into(),
                    key: None,
                    context: "Workspace".into(),
                    palette: false,
                    requires_args: false,
                    args: None,
                    cli: false,
                    handler: Rc::new(move |_, _, _| {
                        api::Handled::Done(gpui::Task::ready(Ok(Value::Null)))
                    }),
                },
            ],
            cx,
        )
        .unwrap();
    });

    let repo_plugins = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
    let spy_reset = spy.clone();
    let hooks = BackdropHooks {
        reset: Some(Rc::new(move || {
            spy_reset.borrow_mut().frame += 1;
        })),
        record: None,
    };

    let shell_holder: Rc<RefCell<Option<Entity<MainShell>>>> = Rc::default();
    let shell_holder_init = shell_holder.clone();
    let (_, cx) = cx.add_window_view(|window, cx| {
        let shell = cx.new(|cx| {
            let shell = MainShell::new(
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
            );
            let spy_buddy = spy.clone();
            let app = shell.app_view().read(cx);
            app.register_component(
                testing::BUDDY,
                Rc::new(move |ctx| {
                    let mut log = spy_buddy.borrow_mut();
                    log.next += 1;
                    let id = log.next;
                    log.live.insert(id, Seen::default());
                    Box::new(SpyBuddy {
                        id,
                        log: spy_buddy.clone(),
                        inner: testing::make_buddy(ctx),
                    })
                }),
            );
            shell
        });
        *shell_holder_init.borrow_mut() = Some(shell.clone());
        WorkspaceHarness {
            focus: cx.focus_handle(),
            shell,
        }
    });
    let shell = shell_holder.borrow().clone().unwrap();
    cx.simulate_resize(size(px(1200.), px(800.)));

    assert!(
        wait_until(cx, Duration::from_secs(15), |cx| {
            shell.read_with(cx, |s, _| s.initial_plugins_settled())
                && cx.update(|_, cx| api::layout("smelt.layouts/standard", cx).is_some())
        }),
        "内置插件未初始化完成"
    );

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

    assert!(
        wait_until(cx, Duration::from_secs(15), |_| {
            buddy_bounds(&spy, "conv-a").is_some() && buddy_bounds(&spy, "conv-b").is_some()
        }),
        "侧栏会话未渲染"
    );

    let a_rect = buddy_bounds(&spy, "conv-a").expect("conv-a bounds");
    let b_rect = buddy_bounds(&spy, "conv-b").expect("conv-b bounds");
    let row_a = point(a_rect.center().x + px(50.), a_rect.center().y);
    let row_b = point(b_rect.center().x + px(50.), b_rect.center().y);
    let empty_sidebar = point(px(120.), b_rect.bottom() + px(120.));
    let center_native = point(px(700.), px(400.));

    let press_escape = |cx: &mut VisualTestContext| {
        cx.simulate_keystrokes("escape");
        // GPUI 的 simulate_keystrokes 只发 KeyDown；补一条 KeyUp，模拟真实键盘松手，
        // 避免后续再按 Esc 被 NSH keymap 判定为长按自动连发。
        cx.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse("escape").unwrap(),
        });
    };

    let open_rename_by_double_click = |cx: &mut VisualTestContext| {
        cx.simulate_mouse_move(row_a, None, Modifiers::default());
        cx.simulate_event(gpui::MouseDownEvent {
            position: row_a,
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
            click_count: 2,
            first_mouse: false,
        });
        cx.simulate_event(gpui::MouseUpEvent {
            position: row_a,
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
            click_count: 2,
        });
        assert!(
            wait_until(cx, Duration::from_secs(3), has_rename_input),
            "双击会话行后应出现值为 Alpha Chat 的重命名输入框"
        );
    };

    // 1. 双击进入重命名 -> 点输入框内部不应关闭 -> 改字后按 Esc 应取消且不调用 workspace.rename。
    open_rename_by_double_click(cx);
    cx.simulate_click(row_a, Modifiers::default());
    cx.run_until_parked();
    cx.update(|w, _| w.refresh());
    assert!(
        has_rename_input(cx),
        "点击重命名输入框内部不应触发外部点击关闭"
    );
    cx.simulate_keystrokes("x");
    assert!(
        wait_until(cx, Duration::from_secs(2), |cx| {
            rename_input_node(cx)
                .and_then(|n| {
                    n.pointer("/aria/value")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .is_some_and(|v| v.contains('x'))
        }),
        "重命名输入框应能正常接收编辑输入"
    );
    press_escape(cx);
    assert!(
        wait_until(cx, Duration::from_secs(2), |cx| !has_rename_input(cx)),
        "双击重命名后按 Esc 应取消并关闭输入框"
    );
    assert!(
        rename_calls.lock().unwrap().is_empty(),
        "按 Esc 取消重命名不应调用 workspace.rename：{:?}",
        *rename_calls.lock().unwrap()
    );

    // 2. 右键菜单「Rename」-> 立刻按 Esc（无需先敲其它键）应取消且不提交。
    cx.simulate_mouse_move(row_a, None, Modifiers::default());
    cx.simulate_mouse_down(row_a, MouseButton::Right, Modifiers::default());
    cx.simulate_mouse_up(row_a, MouseButton::Right, Modifiers::default());
    assert!(
        wait_until(cx, Duration::from_secs(2), |cx| {
            has_a11y_label(cx, "Rename")
        }),
        "右键会话行应弹出包含 Rename 的菜单"
    );
    let rename_pos = point(row_a.x + px(40.), row_a.y + px(46.));
    cx.simulate_mouse_move(rename_pos, None, Modifiers::default());
    cx.simulate_click(rename_pos, Modifiers::default());
    assert!(
        wait_until(cx, Duration::from_secs(2), has_rename_input),
        "点击右键菜单 Rename 后应打开行内重命名输入框"
    );
    press_escape(cx);
    assert!(
        wait_until(cx, Duration::from_secs(2), |cx| !has_rename_input(cx)),
        "右键菜单进入重命名后按 Esc 应立即取消并关闭输入框"
    );
    assert!(
        rename_calls.lock().unwrap().is_empty(),
        "右键菜单重命名按 Esc 取消不应调用 workspace.rename：{:?}",
        *rename_calls.lock().unwrap()
    );

    // 3. 点击另一条会话行应退出重命名输入框。
    open_rename_by_double_click(cx);
    cx.simulate_click(row_b, Modifiers::default());
    assert!(
        wait_until(cx, Duration::from_secs(2), |cx| !has_rename_input(cx)),
        "点击其它会话行应退出重命名输入框"
    );

    // 4. 点击侧栏空白处应退出重命名输入框。
    open_rename_by_double_click(cx);
    cx.simulate_click(empty_sidebar, Modifiers::default());
    assert!(
        wait_until(cx, Duration::from_secs(2), |cx| !has_rename_input(cx)),
        "点击侧栏空白处应退出重命名输入框"
    );

    // 5. 点击右侧原生视图（GPUI 焦点移出 NSH AppView）应退出重命名输入框。
    open_rename_by_double_click(cx);
    cx.simulate_click(center_native, Modifiers::default());
    assert!(
        wait_until(cx, Duration::from_secs(2), |cx| !has_rename_input(cx)),
        "点击右侧原生视图应退出重命名输入框"
    );
    assert!(
        rename_calls.lock().unwrap().is_empty(),
        "未改动标题时点外部退出不应发送 workspace.rename：{:?}",
        *rename_calls.lock().unwrap()
    );

    // 6. 重命名全选覆盖输入 + 回车应提交新标题并关闭输入框。
    let app_focus = shell.read_with(cx, |s, cx| {
        use gpui::Focusable as _;
        s.app_view().read(cx).focus_handle(cx)
    });
    open_rename_by_double_click(cx);
    assert!(
        wait_until(cx, Duration::from_secs(2), |cx| {
            cx.update(|window, _| app_focus.is_focused(window))
        }),
        "双击进入重命名后主壳 AppView 应拿回 GPUI 窗口焦点"
    );
    cx.simulate_keystrokes("z");
    assert!(
        wait_until(cx, Duration::from_secs(2), |cx| {
            rename_input_node(cx)
                .and_then(|n| {
                    n.pointer("/aria/value")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .as_deref()
                == Some("z")
        }),
        "进入重命名聚焦后应全选原标题，直接键入应覆盖为 z，实际节点：{:?}",
        rename_input_node(cx)
    );
    cx.simulate_keystrokes("enter");
    assert!(
        wait_until(cx, Duration::from_secs(2), |cx| !has_rename_input(cx)),
        "按回车应关闭重命名输入框"
    );
    assert!(
        wait_until(cx, Duration::from_secs(2), |_| {
            rename_calls.lock().unwrap().len() == 1
        }),
        "按回车提交新标题应调用一次 workspace.rename"
    );
    assert_eq!(
        rename_calls.lock().unwrap()[0]["params"]["title"].as_str(),
        Some("z")
    );

    // 7. 重命名期间悬停在行首小人头像上也不应弹出子 Tooltip 吃掉 Esc。
    open_rename_by_double_click(cx);
    cx.simulate_mouse_move(a_rect.center(), None, Modifiers::default());
    cx.run_until_parked();
    cx.update(|w, _| w.refresh());
    press_escape(cx);
    assert!(
        wait_until(cx, Duration::from_secs(2), |cx| !has_rename_input(cx)),
        "重命名期间悬停在行首头像上按 Esc 应一次取消"
    );

    // 8. 侧栏根节点 focus-list（⌘⇧E）发出的 view_opened({ focus: true }) 应把 key_focus 交给
    //    session-list-root，随后 Cmd+Down 应能直接切会话。
    cx.simulate_click(center_native, Modifiers::default());
    cx.run_until_parked();
    open_view_calls.lock().unwrap().clear();
    cx.update(|_, cx| {
        api::run("smelt.session-list/focus-list", Value::Null, cx).detach();
    });
    assert!(
        wait_until(cx, Duration::from_secs(2), |_| {
            open_view_calls.lock().unwrap().iter().any(|args| {
                args["view"].as_str() == Some("smelt.session-list/main")
                    && args["args"]["focus"].as_bool() == Some(true)
            })
        }),
        "调用 focus-list 应请求打开 smelt.session-list/main 并带上 focus: true"
    );
    open_view_calls.lock().unwrap().clear();
    // 按宿主真实路径派发 view_opened({ focus: true }) 给 session-list 视图代理。
    cx.update(|window, cx| {
        let spec = api::view("smelt.session-list/main", cx).expect("session-list view spec");
        let handle = (spec.build)(window, cx);
        handle.open(&json!({ "focus": true }), window, cx);
    });
    assert!(
        wait_until(cx, Duration::from_secs(2), |cx| {
            cx.update(|window, _| app_focus.is_focused(window))
        }),
        "view_opened({{ focus: true }}) 后主壳 AppView 应拿到窗口焦点"
    );
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-down"
    } else {
        "ctrl-down"
    });
    assert!(
        wait_until(cx, Duration::from_secs(2), |_| {
            open_view_calls
                .lock()
                .unwrap()
                .iter()
                .any(|args| args["view"].as_str() == Some("smelt.sessions/session"))
        }),
        "focus-list 后在侧栏按 Cmd+Down 应触发 step_session 打开会话，实际：{:?}",
        *open_view_calls.lock().unwrap()
    );

    // 9. ⌘N 新建会话面板打开后无需先点击，直接按 Enter 即可通过 MenuBody 键盘导航启动。
    create_calls.lock().unwrap().clear();
    cx.update(|_, cx| {
        api::run("smelt.session-list/new-session", Value::Null, cx).detach();
    });
    assert!(
        wait_until(cx, Duration::from_secs(2), |cx| {
            has_a11y_label(cx, "New Session") && has_a11y_label(cx, "Terminal")
        }),
        "调用 new-session 应打开新建会话面板并加载目录，当前 labels：{:?}",
        a11y_nodes(cx)
            .into_iter()
            .filter_map(|n| n
                .pointer("/aria/label")
                .and_then(Value::as_str)
                .map(str::to_owned))
            .collect::<Vec<_>>()
    );
    cx.simulate_keystrokes("enter");
    assert!(
        wait_until(cx, Duration::from_secs(2), |_| {
            !create_calls.lock().unwrap().is_empty()
        }),
        "新建会话面板打开后应立即可用键盘 Enter 启动（MenuBody 自动拿焦）"
    );

    // 10. 右侧文件面板搜索框聚焦键入后按 Esc 应通过 onKeyPress 清空搜索词。
    let files_search_label = "Search file names / content…";
    assert!(
        wait_until(cx, Duration::from_secs(5), |cx| {
            text_input_by_label(cx, files_search_label).is_some()
        }),
        "右侧辅助栏应已挂载文件搜索框"
    );
    // 文件搜索框位于右侧辅助栏顶部（34px 标签栏下方）。
    cx.simulate_click(point(px(1060.), px(54.)), Modifiers::default());
    cx.simulate_keystrokes("f o o");
    assert!(
        wait_until(cx, Duration::from_secs(2), |cx| {
            text_input_by_label(cx, files_search_label)
                .and_then(|n| {
                    n.pointer("/aria/value")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .as_deref()
                == Some("foo")
        }),
        "文件搜索框应能正常键入 foo，实际：{:?}",
        text_input_by_label(cx, files_search_label)
    );
    press_escape(cx);
    assert!(
        wait_until(cx, Duration::from_secs(2), |cx| {
            text_input_by_label(cx, files_search_label)
                .and_then(|n| {
                    n.pointer("/aria/value")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .as_deref()
                == Some("")
        }),
        "文件搜索框按 Esc 应通过 onKeyPress 清空搜索词，实际：{:?}",
        text_input_by_label(cx, files_search_label)
    );

    // 11. 远程机器页自定义 Relay 输入后不敲回车直接点外部，也应自动防抖/失焦落盘保存。
    shell.update(cx, |shell, cx| {
        shell.set_active_page(
            "smelt.layouts/workbench".into(),
            Some(OutletTarget {
                id: "smelt.machines/remote".into(),
                args: Value::Null,
                seq: 3,
                managed_rust_instance: false,
            }),
            cx,
        );
    });
    assert!(
        wait_until(cx, Duration::from_secs(5), |cx| {
            text_input_by_label(cx, "Custom relay")
                .is_some_and(|n| n.pointer("/aria/disabled").and_then(Value::as_bool) != Some(true))
        }),
        "切换到 machines/remote 后自定义 relay 输入框应加载完成并可编辑"
    );
    // 用键盘 Tab 循焦点链到达 Custom relay（通过 a11y gpui_focus 精确确认，不依赖滚动与像素坐标）。
    let mut focused_relay = false;
    for _ in 0..48 {
        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        if focused_text_input_label(cx).as_deref() == Some("Custom relay") {
            focused_relay = true;
            break;
        }
    }
    assert!(
        focused_relay,
        "应能通过键盘 Tab 聚焦 Custom relay 输入框，当前焦点：{:?}",
        focused_text_input_label(cx)
    );
    cx.simulate_keystrokes("r e l a y");
    assert!(
        wait_until(cx, Duration::from_secs(2), |cx| {
            text_input_by_label(cx, "Custom relay")
                .and_then(|n| {
                    n.pointer("/aria/value")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .as_deref()
                == Some("relay")
        }),
        "聚焦后应能键入 Custom relay 输入框，实际：{:?}",
        text_input_by_label(cx, "Custom relay")
    );
    // 不按回车，直接点击侧栏空白（验证无需 onSubmit 也能自动落盘保存）。
    cx.simulate_click(empty_sidebar, Modifiers::default());
    assert!(
        wait_until(cx, Duration::from_secs(3), |_| {
            remote_config_sets
                .lock()
                .unwrap()
                .iter()
                .any(|req| req["params"]["iroh_relay"].as_str() == Some("relay"))
        }),
        "修改 Custom relay 后不按回车直接点外面也应通过防抖/失焦自动保存：{:?}",
        *remote_config_sets.lock().unwrap()
    );

    shell.update(cx, |shell, cx| shell.shutdown(cx));
}
