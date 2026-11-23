//! 主壳端到端 GPUI 交互回归测试（独立进程，避开 `nsh_platform_gpui::rt::JOBS` 单进程 `OnceLock`）：
//! 真实加载仓库 `plugins/`（纯 NSH `<Icon>` 与 `<SmeltButton>`，不经任何测试插桩），验证：
//! 1. 本机上线后事件驱动唤醒 `waitOnline`，底部栏渲染出 Claude 额度芯片；
//! 2. 悬停展开的项目行触发 `icons/folder-open.svg` -> 折叠箭头切换；
//! 3. 悬停会话行右侧露出关闭按钮；
//! 4. 拖拽会话行调整顺序且不误触发打开会话。

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
    AnyElement, AnyWindowHandle, App, AppContext as _, Bounds, Context, IntoElement, Modifiers,
    MouseButton, ParentElement as _, Pixels, Render, ScrollDelta, ScrollEvent, Styled as _,
    TestAppContext, VisualTestContext, Window, canvas, div, point, px, size,
};
use nsh_platform_gpui::NativeComponent;
use serde_json::{Value, json};
use smelt_nsh::{
    BackdropHooks, MainShell, OutletTarget, PluginSource, ShellConfig, ShellLabels, SourceDir,
    WindowCorners, testing,
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

struct InteractionHost {
    local_online: Arc<AtomicBool>,
    rpc_methods: Arc<Mutex<Vec<String>>>,
    toggled_pins: Arc<Mutex<Vec<String>>>,
}

impl Host for InteractionHost {
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
        let toggled_pins = self.toggled_pins.clone();
        Some(RuntimeClient::new(move || {
            let (client, server) = RuntimeStream::pair()?;
            let methods = methods.clone();
            let toggled_pins = toggled_pins.clone();
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
                                                "used_percent": 38.0,
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
                    "remote.config.get" => json!({ "enabled": false, "iroh_relay": "" }),
                    "new_session.catalog" => {
                        let mut terminals = vec![json!({
                            "key": "term:zsh",
                            "kind": "terminal",
                            "label": "Zsh",
                            "target": "blankTerminal",
                            "pinned": false
                        })];
                        for i in 1..=17 {
                            terminals.push(json!({
                                "key": format!("term:tool-{i}"),
                                "kind": "terminal",
                                "label": format!("Tool {i}"),
                                "target": "blankTerminal",
                                "pinned": false
                            }));
                        }
                        json!({
                            "sections": {
                                "common": [],
                                "terminal": terminals,
                                "conversation": []
                            }
                        })
                    }
                    "new_session.pins.toggle" => {
                        if let Some(key) = req["params"]["key"].as_str() {
                            toggled_pins.lock().unwrap().push(key.to_owned());
                        }
                        json!({ "pinned": true })
                    }
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
            projects: vec!["/work/smelt".into(), "/work/other".into()],
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

fn has_a11y(cx: &mut VisualTestContext, role: Option<&str>, label: &str) -> bool {
    a11y_labels(cx)
        .into_iter()
        .any(|(r, l)| l == label && role.is_none_or(|want| r == want))
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
        if start.elapsed() >= Duration::from_secs(15) {
            panic!("未等到：{what}；当前 a11y：{:?}", a11y_labels(cx));
        }
        // 轮询例外：测试平台等待引擎线程提交帧并完成重绘；周期 15ms，上限 15s。
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
fn main_shell_hover_drag_and_quota_regressions(cx: &mut TestAppContext) {
    // 独立测试进程单测试，开启 GPUI a11y 树捕获以直接校验一等 NSH 节点可见性。
    unsafe {
        std::env::set_var("GPUI_A11Y_ACTIVE", "1");
    }

    let local_online = Arc::new(AtomicBool::new(false));
    let rpc_methods = Arc::new(Mutex::new(Vec::<String>::new()));
    let toggled_pins = Arc::new(Mutex::new(Vec::<String>::new()));
    let opened_sessions = Rc::new(RefCell::new(Vec::<Value>::new()));
    let spy = Rc::new(RefCell::new(SpyLog::default()));

    cx.executor().allow_parking();
    cx.update(|cx| {
        smelt_ui::init(cx);
        api::set_locale("en", cx);
        api::install(
            InteractionHost {
                local_online: local_online.clone(),
                rpc_methods: rpc_methods.clone(),
                toggled_pins: toggled_pins.clone(),
            },
            cx,
        );
        api::register_views(
            "smelt",
            vec![stub_spec("outlet"), stub_spec("settings")],
            cx,
        )
        .unwrap();
        let opened = opened_sessions.clone();
        api::register_commands(
            "smelt.app",
            "App",
            vec![api::CommandSpec {
                name: "open-view".into(),
                title: "Open view".into(),
                key: None,
                context: "Workspace".into(),
                palette: false,
                requires_args: false,
                args: None,
                cli: false,
                handler: Rc::new(move |args, _, _| {
                    if args["view"].as_str() == Some("smelt.sessions/session") {
                        opened.borrow_mut().push(args.clone());
                    }
                    api::Handled::Done(gpui::Task::ready(Ok(json!(true))))
                }),
            }],
            cx,
        )
        .unwrap();
    });

    let repo_plugins = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
    let spy_reset = spy.clone();
    let backdrop_regions: Rc<RefCell<std::collections::BTreeMap<u8, Option<Bounds<Pixels>>>>> =
        Rc::default();
    let r_reset = backdrop_regions.clone();
    let r_record = backdrop_regions.clone();
    let placement_key = |p: smelt_nsh::BackdropPlacement| match p {
        smelt_nsh::BackdropPlacement::Center => 0u8,
        smelt_nsh::BackdropPlacement::Left => 1,
        smelt_nsh::BackdropPlacement::Right => 2,
        smelt_nsh::BackdropPlacement::Bottom => 3,
    };
    let hooks = BackdropHooks {
        reset: Some(Rc::new(move || {
            spy_reset.borrow_mut().frame += 1;
            r_reset.borrow_mut().clear();
        })),
        record: Some(Rc::new(move |placement, bounds, _| {
            r_record
                .borrow_mut()
                .insert(placement_key(placement), bounds);
        })),
    };

    let (shell, cx) = cx.add_window_view(|window, cx| {
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
    cx.simulate_resize(size(px(1200.), px(800.)));

    wait_for(cx, "内置插件初始化完毕", |cx| {
        shell.read_with(cx, |s, _| s.initial_plugins_settled())
            && cx.update(|_, cx| api::layout("smelt.layouts/standard", cx).is_some())
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
                id: "smelt.sessions/session".into(),
                args: Value::Null,
                seq: 1,
                managed_rust_instance: false,
            }),
            cx,
        );
    });

    wait_for(cx, "侧栏项目与两条会话渲染完成", |cx| {
        buddy_bounds(&spy, "conv-a").is_some()
            && buddy_bounds(&spy, "conv-b").is_some()
            && has_a11y(cx, None, "icons/folder-open.svg")
            && !has_a11y(cx, Some("Button"), "Collapse")
            && !has_a11y(cx, Some("Button"), "Close")
    });

    // 1. 额度查询：本机上线后事件唤醒 waitOnline，底部栏渲染出 Claude 额度芯片。
    local_online.store(true, Ordering::SeqCst);
    cx.update(|_, cx| api::emit(HostEvent::MachinesChanged, cx));
    wait_for(cx, "额度栏渲染出 Claude 芯片", |cx| {
        has_a11y(cx, None, "Claude")
    });

    // 2. 悬停项目行：icons/folder-open.svg 隐藏、折叠箭头按钮露出来。
    let a_rect = buddy_bounds(&spy, "conv-a").expect("conv-a bounds");
    let b_rect = buddy_bounds(&spy, "conv-b").expect("conv-b bounds");
    assert!(
        a_rect.origin.y < b_rect.origin.y,
        "初始顺序应为 conv-a 在 conv-b 上方"
    );
    let project_hover = point(a_rect.origin.x + px(40.), a_rect.origin.y - px(16.));
    cx.simulate_mouse_move(project_hover, None, Modifiers::default());
    wait_for(
        cx,
        "悬停项目行后 folder-open 隐藏、Collapse 箭头露出",
        |cx| {
            !has_a11y(cx, None, "icons/folder-open.svg") && has_a11y(cx, Some("Button"), "Collapse")
        },
    );

    // 3. 悬停会话行：右侧露出纯 NSH SmeltButton 关闭按钮。
    cx.simulate_mouse_move(
        point(a_rect.center().x + px(40.), a_rect.center().y),
        None,
        Modifiers::default(),
    );
    wait_for(cx, "悬停会话行后露出 Close 按钮", |cx| {
        has_a11y(cx, Some("Button"), "Close")
    });

    // 4. 拖拽重排：把 conv-b 拖到 conv-a 上半区，顺序翻转且不误触打开会话。
    opened_sessions.borrow_mut().clear();
    let drag_start = point(b_rect.center().x + px(40.), b_rect.center().y);
    let drag_mid = point(a_rect.center().x + px(40.), a_rect.center().y + px(4.));
    let drag_target = point(a_rect.center().x + px(40.), a_rect.origin.y + px(3.));
    cx.simulate_mouse_down(drag_start, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(drag_mid, Some(MouseButton::Left), Modifiers::default());
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.simulate_mouse_move(drag_target, Some(MouseButton::Left), Modifiers::default());
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.simulate_mouse_up(drag_target, MouseButton::Left, Modifiers::default());

    wait_for(cx, "拖拽后 conv-b 排到 conv-a 上方", |_| {
        matches!(
            (buddy_bounds(&spy, "conv-a"), buddy_bounds(&spy, "conv-b")),
            (Some(a), Some(b)) if b.origin.y < a.origin.y
        )
    });
    assert!(
        opened_sessions.borrow().is_empty(),
        "拖拽松手不应误触发打开会话：{:?}",
        *opened_sessions.borrow()
    );

    // 5. 红绿灯旁的左抽屉开关按钮：点击收起、再点击展开，同帧同步材质分块，无重复抖动。
    let layout_changes = Rc::new(std::cell::Cell::new(0usize));
    let lc = layout_changes.clone();
    let _sub = cx.update(|_, cx| {
        cx.subscribe(&shell, move |_, event: &smelt_nsh::MainShellEvent, _| {
            if matches!(event, smelt_nsh::MainShellEvent::LayoutChanged { .. }) {
                lc.set(lc.get() + 1);
            }
        })
    });
    let left_toggle = point(px(78. + 10.), px(17.));
    cx.simulate_mouse_move(left_toggle, None, Modifiers::default());
    cx.simulate_click(left_toggle, Modifiers::default());
    wait_for(
        cx,
        "点击左抽屉开关后收起左侧栏并同步更新材质分块",
        |cx| {
            let regions = backdrop_regions.borrow();
            let left_cleared = regions.get(&1).copied().flatten().is_none();
            let center_expanded = regions
                .get(&0)
                .copied()
                .flatten()
                .is_some_and(|b| b.origin.x == px(0.) && b.size.width > px(800.));
            shell.read_with(cx, |s, _| s.shell_view().left_dock == Some(false))
                && has_a11y(cx, Some("Button"), "Expand left sidebar")
                && left_cleared
                && center_expanded
        },
    );
    assert_eq!(
        layout_changes.get(),
        1,
        "单次收起不应触发多次 LayoutChanged"
    );

    cx.simulate_click(left_toggle, Modifiers::default());
    wait_for(
        cx,
        "再次点击左抽屉开关后展开左侧栏并恢复材质分块",
        |cx| {
            let regions = backdrop_regions.borrow();
            let left_open = regions
                .get(&1)
                .copied()
                .flatten()
                .is_some_and(|b| b.origin.x == px(0.) && b.size.width > px(200.));
            let center_shifted = regions
                .get(&0)
                .copied()
                .flatten()
                .is_some_and(|b| b.origin.x > px(200.));
            shell.read_with(cx, |s, _| s.shell_view().left_dock == Some(true))
                && has_a11y(cx, Some("Button"), "Collapse left sidebar")
                && left_open
                && center_shifted
        },
    );
    assert_eq!(
        layout_changes.get(),
        2,
        "再展开应只多触发一次 LayoutChanged"
    );

    // 6. 项目行「+」按钮：点击弹出新建会话弹层，再次点击收起（不能开完立刻被 Popover 自己关掉）。
    // 左停靠区宽 260，列表水平内边距 8，行右内边距 4，20x20 按钮中心在 x=238。
    let plus_pos = point(px(238.), a_rect.origin.y - px(22.));
    cx.simulate_mouse_move(plus_pos, None, Modifiers::default());
    wait_for(
        cx,
        "悬停到项目行 + 号所在行后 Collapse 露出",
        |cx| has_a11y(cx, Some("Button"), "Collapse"),
    );
    cx.simulate_click(plus_pos, Modifiers::default());
    wait_for(
        cx,
        "点击项目行 + 号弹出新建会话菜单并列出 Zsh",
        |cx| has_a11y(cx, Some("Button"), "Zsh"),
    );

    // 7. 滚动新建会话弹层后再悬停项，Scroll 不能重建归零，星标按钮应落在滚动条槽位左侧且命中滚动后的行。
    let popup_mid = point(px(340.), a_rect.origin.y + px(80.));
    cx.simulate_mouse_move(popup_mid, None, Modifiers::default());
    for _ in 0..5 {
        cx.simulate_event(ScrollEvent {
            position: popup_mid,
            delta: ScrollDelta::Pixels(point(px(0.), px(-28.))),
            ..Default::default()
        });
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
    }
    let hovered_row = point(px(340.), a_rect.origin.y + px(15.));
    cx.simulate_mouse_move(hovered_row, None, Modifiers::default());
    wait_for(
        cx,
        "悬停滚动后的菜单行露出 Add to Favorites 星标按钮",
        |cx| has_a11y(cx, Some("Button"), "Add to Favorites"),
    );
    let star_btn = point(px(511.), a_rect.origin.y + px(15.));
    cx.simulate_mouse_move(star_btn, None, Modifiers::default());
    cx.simulate_click(star_btn, Modifiers::default());
    wait_for(cx, "点击星标触发 new_session.pins.toggle", |_| {
        !toggled_pins.lock().unwrap().is_empty()
    });
    let pinned_key = toggled_pins.lock().unwrap()[0].clone();
    assert!(
        pinned_key.starts_with("term:tool-"),
        "滚动后悬停不应把列表重置回顶部 Zsh，实际命中：{pinned_key}"
    );

    cx.simulate_click(plus_pos, Modifiers::default());
    wait_for(
        cx,
        "再次点击项目行 + 号收起新建会话菜单",
        |cx| !has_a11y(cx, Some("Button"), "Zsh"),
    );

    shell.update(cx, |shell, cx| shell.shutdown(cx));
}
