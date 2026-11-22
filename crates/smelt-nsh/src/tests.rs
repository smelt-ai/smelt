//! 试点这一整套：真外壳、`pilot-plugins/` 里真的插件（launcher + session-list + buddies）、
//! 真的原生组件、真的 SQLite `PluginStore`、真的文件监视，在测试平台的窗口里跑。
//! 终端换成一块记按下次数的假视图；宿主状态和动作由测试桩接管，不建假宿主。

use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{
    AnyElement, App, Bounds, Entity, InteractiveElement as _, IntoElement as _, Modifiers,
    MouseButton, ParentElement as _, Pixels, Styled as _, TestAppContext, VisualTestContext,
    Window, canvas, div, point, px, size,
};
use nsh_platform_gpui::{AppOptions, AppView, Limits, NativeComponent};
use serde_json::{Value, json};
use smelt_plugin_api::client::protocol::control::ControlMethod;

use super::*;

const PEOPLE: usize = 100;
const HALF: usize = PEOPLE / 2;

/// 一个建着的组件：它的 prop、画在哪。
#[derive(Default)]
struct Seen {
    kind: &'static str,
    props: BTreeMap<String, Value>,
    bounds: Option<Bounds<Pixels>>,
}

#[derive(Default)]
struct Log {
    live: BTreeMap<usize, Seen>,
    next: usize,
    terminal_presses: usize,
}

/// 套在真组件外面记一笔；`inner` 为空的是假终端。
struct Recorded {
    id: usize,
    log: Rc<RefCell<Log>>,
    inner: Option<Box<dyn NativeComponent>>,
}

impl NativeComponent for Recorded {
    fn render(&mut self, window: &mut Window, cx: &mut App) -> Option<AnyElement> {
        let content = match &mut self.inner {
            Some(inner) => inner.render(window, cx)?,
            None => {
                let log = self.log.clone();
                div()
                    .size_full()
                    .on_mouse_down(MouseButton::Left, move |_, _, _| {
                        log.borrow_mut().terminal_presses += 1;
                    })
                    .into_any_element()
            }
        };
        let (id, log) = (self.id, self.log.clone());
        let measured = canvas(
            move |bounds, _, _| {
                if let Some(seen) = log.borrow_mut().live.get_mut(&id) {
                    seen.bounds = Some(bounds);
                }
            },
            |_, _, _, _| {},
        );
        Some(
            div()
                .size_full()
                .relative()
                .child(content)
                .child(measured.absolute().top_0().left_0().size_full())
                .into_any_element(),
        )
    }

    fn set_prop(&mut self, name: &str, value: Option<&Value>, window: &mut Window, cx: &mut App) {
        if let Some(seen) = self.log.borrow_mut().live.get_mut(&self.id) {
            seen.props
                .insert(name.to_owned(), value.cloned().unwrap_or(Value::Null));
        }
        if let Some(inner) = &mut self.inner {
            inner.set_prop(name, value, window, cx);
        }
    }
}

impl Drop for Recorded {
    fn drop(&mut self) {
        self.log.borrow_mut().live.remove(&self.id);
    }
}

#[derive(Default)]
struct ActionLog {
    runs: Vec<(String, Value)>,
    calls: Vec<(ControlMethod, Value)>,
    clipboard: Vec<String>,
    topics: Vec<(String, Vec<String>)>,
}

struct Bench<'a> {
    cx: &'a mut VisualTestContext,
    _app: Entity<AppView>,
    log: Rc<RefCell<Log>>,
    sources: PathBuf,
    plugin_host: PluginHost,
    bridge: host::Bridge,
    actions: RefCell<mpsc::UnboundedReceiver<host::Action>>,
    action_log: RefCell<ActionLog>,
    running_conv: RefCell<String>,
    active_session: RefCell<Option<String>>,
}

impl Bench<'_> {
    fn pump_actions(&self) {
        let mut rx = self.actions.borrow_mut();
        while let Ok(action) = rx.try_recv() {
            match action {
                host::Action::Run {
                    command,
                    args,
                    call,
                } => {
                    self.action_log
                        .borrow_mut()
                        .runs
                        .push((command.clone(), args.clone()));
                    match command.as_str() {
                        "smelt.sessions/selection" => call.resolve(json!({ "kind": "none" })),
                        "smelt.agents/agent-settings" => call.resolve(json!([])),
                        "smelt.app/show-main-window" => call.resolve(Value::Null),
                        "smelt.app/open-view" => {
                            if args["view"].as_str() == Some("smelt.sessions/session")
                                && let Some(conv_id) = args["args"]["conversation_id"].as_str()
                            {
                                let sid = conv_id.replacen('c', "s", 1);
                                *self.active_session.borrow_mut() = Some(sid.clone());
                                let running = self.running_conv.borrow().clone();
                                self.bridge
                                    .push(&self.plugin_host, state(&running, Some(sid.as_str())));
                            }
                            call.resolve(json!(true));
                        }
                        _ => call.resolve(Value::Null),
                    }
                }
                host::Action::Call {
                    method,
                    params,
                    call,
                    ..
                } => {
                    self.action_log.borrow_mut().calls.push((method, params));
                    call.resolve(Value::Null);
                }
                host::Action::ContextMenuItems {
                    menu,
                    context,
                    call,
                    ..
                } => {
                    let item = json!({
                        "label": "Open in Test Editor",
                        "icon": "icons/external-link.svg",
                        "image": null,
                        "command": "test.ext/open",
                        "args": { "menu": menu, "context": context },
                    });
                    call.resolve(json!([item]));
                }
                host::Action::ClipboardWrite { text, call } => {
                    self.action_log.borrow_mut().clipboard.push(text);
                    call.resolve(Value::Null);
                }
                host::Action::RegisterTopics {
                    plugin,
                    topics,
                    call,
                } => {
                    self.action_log.borrow_mut().topics.push((plugin, topics));
                    call.resolve(Value::Null);
                }
            }
        }
    }

    fn wait(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
        let start = Instant::now();
        loop {
            self.pump_actions();
            self.cx.run_until_parked();
            self.cx.update(|window, _| window.refresh());
            self.cx.run_until_parked();
            self.pump_actions();
            if done(self) {
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "没等到{what}；建着的：{:?}；runs：{:?}；calls：{:?}；topics：{:?}",
                self.kinds(),
                self.action_log.borrow().runs,
                self.action_log.borrow().calls,
                self.action_log.borrow().topics,
            );
            // 轮询例外：测试平台没有「引擎线程提交了一帧」的事件可等；周期 10ms，上限 20s。
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn kinds(&self) -> BTreeMap<&'static str, usize> {
        let mut kinds = BTreeMap::new();
        for seen in self.log.borrow().live.values() {
            *kinds.entry(seen.kind).or_default() += 1;
        }
        kinds
    }

    /// 建着的 `kind` 里 `prop` 是 `value` 的。
    fn find(&self, kind: &str, prop: &str, value: Value) -> Vec<usize> {
        self.log
            .borrow()
            .live
            .iter()
            .filter(|(_, seen)| seen.kind == kind && seen.props.get(prop) == Some(&value))
            .map(|(id, _)| *id)
            .collect()
    }

    fn all(&self, kind: &str) -> Vec<usize> {
        self.log
            .borrow()
            .live
            .iter()
            .filter(|(_, seen)| seen.kind == kind)
            .map(|(id, _)| *id)
            .collect()
    }

    fn bounds(&self, id: usize) -> Option<Bounds<Pixels>> {
        self.log
            .borrow()
            .live
            .get(&id)?
            .bounds
            .filter(|bounds| bounds.size.width > px(0.) && bounds.size.height > px(0.))
    }

    fn button(&mut self, label: &str) -> Bounds<Pixels> {
        let found = |bench: &Self| {
            bench
                .find(button::NAME, "label", json!(label))
                .first()
                .and_then(|id| bench.bounds(*id))
        };
        self.wait(label, |bench| found(bench).is_some());
        found(self).unwrap()
    }

    fn edit(&self, plugin: &str, from: &str, to: &str) {
        let path = self.sources.join(plugin).join("main.tsx");
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains(from), "{plugin} 里没有 {from}");
        fs::write(path, text.replace(from, to)).unwrap();
    }
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn state(running: &str, active_sid: Option<&str>) -> Value {
    state_with(PEOPLE, running, active_sid)
}

fn state_with(count: usize, running: &str, active_sid: Option<&str>) -> Value {
    let half = count / 2;
    let projects = ["/work/alpha", "/work/beta"];
    let conversations: Vec<Value> = (0..count)
        .map(|n| {
            let root = if n < half { projects[0] } else { projects[1] };
            json!({
                "id": format!("c{n}"),
                "acp": {
                    "sid": format!("s{n}"),
                    "cwd": root,
                },
                "terminals": [],
                "last_updated_at": 1_700_000_000 + n as i64,
            })
        })
        .collect();
    let facts: Vec<Value> = (0..count)
        .map(|n| {
            let id = format!("c{n}");
            let root = if n < half { projects[0] } else { projects[1] };
            json!({
                "id": id,
                "name": format!("Chat {n}"),
                "status": if id == running { "running" } else { "idle" },
                "agent": null,
                "automation": false,
                "cwd": root,
                "placement": { "kind": "project", "root": root, "workset": null },
            })
        })
        .collect();
    let active_session = active_sid
        .map(|sid| json!({ "machine": null, "id": sid, "at_latest": true }))
        .unwrap_or(Value::Null);
    json!({
        "locale": "en",
        "active_context": { "project": null, "session": active_session },
        "connections": { "remotes": [], "load_error": null },
        "machines": [{
            "machine": null,
            "name": "This Mac",
            "unavailable": null,
            "path_style": "posix",
            "directory": {
                "epoch": 1,
                "revision": 1,
                "projects": projects,
                "worksets": [],
                "repositories": [],
                "conversations": conversations,
            },
            "states": [],
            "facts": facts,
        }],
        "pages": [],
        "views": [{
            "id": "test/panel", "title": "Panel", "icon": "icons/panel.svg", "settings": null,
            "instances": false, "fills": [], "badge": null, "laid_out": false, "shown": false,
        }],
        "commands": [
            { "id": "smelt.agents/new-conversation", "title": "New chat" },
            { "id": "smelt.app/settings", "title": "Open settings" },
        ],
        "bar_insets": {
            "none": { "left": 8, "right": 8 },
            "bottom-left": { "left": 8, "right": 8 },
            "bottom-right": { "left": 8, "right": 8 },
        },
        "theme": {
            "rem": 16,
            "palette": {
                "bg": "#161618", "side": "#1a1a1d", "surface": "#222226", "soft": "#2a2a30",
                "text": "#eeeeee", "muted": "#888888", "muted_selected": "#aaaaaa",
                "faint": "#666666", "faint_selected": "#999999",
                "ink": "#eeeeee", "on_ink": "#161618", "link": "#3b82f6",
                "run": "#3b82f6", "need": "#f59e0b", "ok": "#22c55e",
                "bad": "#ff0000", "on_bad": "#ffffff",
            },
            "type": {
                "ui": { "size": 13, "line_height": 20, "weight": 400 },
                "ui_medium": { "size": 13, "line_height": 20, "weight": 500 },
                "small": { "size": 11, "line_height": 12, "weight": 400 },
                "small_medium": { "size": 11, "line_height": 12, "weight": 500 },
                "text_xsmall": { "size": 12, "line_height": 18, "weight": 400 },
                "text_xsmall_medium": { "size": 12, "line_height": 18, "weight": 500 },
            },
            "radius": { "sm": 4, "md": 6, "row": 8 },
        },
    })
}

/// 跑完删掉这次的目录。
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[gpui::test]
fn the_pilot_runs_its_plugins_live(cx: &mut TestAppContext) {
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

    cx.executor().allow_parking();
    cx.update(|cx| {
        gpui_base::init(cx);
        smelt_ui::init(cx);
    });
    nsh_platform_gpui::set_ui_sets(vec![nsh_base::registry(), module::registry()]);
    let root = Path::new(env!("OUT_DIR")).join(format!("pilot-test-{}", uuid::Uuid::new_v4()));
    let _scratch = Scratch(root.clone());
    let sources = root.join("sources");
    copy_dir(&plugin_dir(), &sources);

    let log = Rc::new(RefCell::new(Log::default()));
    let (app, cx) =
        cx.add_window_view(|window, cx| AppView::new(AppOptions::default(), window, cx));
    // 原生组件只建看得见的那些：窗口够高，一百行（含两个项目头）才都在。
    cx.simulate_resize(size(px(1000.), px(6200.)));
    let pilot = app.entity_id();
    app.update(cx, |view, _| {
        view.set_limits(Limits {
            memory_bytes: 64 << 20,
            ..Limits::default()
        });
        type Make = Rc<dyn Fn(nsh_platform_gpui::ComponentContext) -> Box<dyn NativeComponent>>;
        let real: [(&'static str, Option<Make>); 5] = [
            (view::NAME, None),
            (button::NAME, Some(Rc::new(button::SmeltButton::make))),
            (components::BUDDY, Some(Rc::new(components::Buddy::make))),
            (components::ICON, Some(Rc::new(components::Icon::make))),
            (
                components::WINDOW_BAR,
                Some(Rc::new(move |cx| components::WindowBar::make(cx, pilot))),
            ),
        ];
        for (kind, make) in real {
            let log = log.clone();
            view.register_component(
                kind,
                Rc::new(move |cx| {
                    let id = {
                        let mut log = log.borrow_mut();
                        log.next += 1;
                        let id = log.next;
                        log.live.insert(
                            id,
                            Seen {
                                kind,
                                ..Seen::default()
                            },
                        );
                        id
                    };
                    Box::new(Recorded {
                        id,
                        log: log.clone(),
                        inner: make.as_ref().map(|make| make(cx)),
                    })
                }),
            );
        }
    });
    type ErrorLog =
        std::sync::Arc<std::sync::Mutex<Vec<(nsh_platform_gpui::AppErrorKind, Option<String>)>>>;
    let errors = ErrorLog::default();
    let plugin_host = PluginHost::new();
    plugin_host.set_plugin_limits(Limits {
        memory_bytes: 64 << 20,
        ..Limits::default()
    });
    let store_hub = store::StoreHub::in_dir(root.join("plugin-data"));
    store_hub.serve(&plugin_host);
    let bridge = host::Bridge::default();
    let actions = bridge.serve(&plugin_host);
    bridge.push(&plugin_host, state("c0", None));
    let _installing = cx.update(|_, cx| {
        let task = boot(
            &app,
            &plugin_host,
            &bridge,
            sources.clone(),
            root.join("stage"),
            cx,
        );
        let err_host = plugin_host.clone();
        let err_log = errors.clone();
        app.update(cx, |view, _| {
            view.on_app_error(move |error| {
                eprintln!("[nsh-pilot] {error}");
                err_log
                    .lock()
                    .unwrap()
                    .push((error.kind, error.plugin.clone()));
                err_host.on_app_error(error);
            });
        });
        task
    });
    let mut bench = Bench {
        cx,
        _app: app,
        log,
        sources,
        plugin_host,
        bridge,
        actions: RefCell::new(actions),
        action_log: RefCell::new(ActionLog::default()),
        running_conv: RefCell::new("c0".to_owned()),
        active_session: RefCell::new(None),
    };

    // 插件挂插件（Shell → session-list VM → buddies VM）：一百行一百个小人，状态是宿主给的那份。
    bench.wait("一百个小人", |b| {
        b.all(components::BUDDY).len() == PEOPLE
            && b.find(components::BUDDY, "seed", json!("c0")).len() == 1
            && !b
                .find(components::ICON, "path", json!("icons/panel.svg"))
                .is_empty()
            && !b
                .find(components::ICON, "path", json!("icons/folder.svg"))
                .is_empty()
    });
    let first = bench.find(components::BUDDY, "seed", json!("c0"))[0];
    let act = |b: &Bench, id: usize| b.log.borrow().live.get(&id)?.props.get("act").cloned();
    bench.wait("c0 在干活", |b| {
        act(b, first).is_some_and(|act| act != "rest")
    });
    let people = bench.all(components::BUDDY);
    *bench.running_conv.borrow_mut() = "c1".to_owned();
    bench.bridge.push(&bench.plugin_host, state("c1", None));
    let second = bench.find(components::BUDDY, "seed", json!("c1"))[0];
    bench.wait("状态变了跟着演", |b| {
        act(b, second).is_some_and(|act| act != "rest")
    });
    assert_eq!(bench.all(components::BUDDY), people, "状态一变重建了小人");

    // session-list 订阅了 attention.changed 话题。
    assert!(
        bench
            .action_log
            .borrow()
            .topics
            .iter()
            .any(|(plugin, topics)| {
                plugin == "session-list" && topics.iter().any(|t| t == "attention.changed")
            }),
        "session-list 未登记 attention.changed 话题"
    );

    // 文案按语言取，按钮点下去宿主收到命令。
    bench.action_log.borrow_mut().runs.clear();
    let settings = bench.button("Settings");
    bench
        .cx
        .simulate_click(settings.center(), Modifiers::default());
    bench.wait("设置那条命令", |b| {
        b.action_log
            .borrow()
            .runs
            .iter()
            .any(|(cmd, _)| cmd == "smelt.app/settings")
    });

    // 行里的小人、图标只是画：点在它们上面也是点这一行。
    bench.action_log.borrow_mut().runs.clear();
    let launcher_icon = bench.find(components::ICON, "path", json!("icons/panel.svg"))[0];
    let at = bench.bounds(launcher_icon).unwrap().center();
    bench.cx.simulate_click(at, Modifiers::default());
    bench.wait("点图标打开那一行", |b| {
        b.action_log.borrow().runs.iter().any(|(cmd, args)| {
            cmd == "smelt.app/open-view" && *args == json!({ "view": "test/panel" })
        })
    });

    let at = bench.bounds(second).unwrap().center();
    bench.cx.simulate_click(at, Modifiers::default());
    bench.wait("点小人选中那段对话", |b| {
        b.all(view::NAME).len() == 1
    });
    let conversation = bench.all(view::NAME)[0];

    // 菜单盖在终端上：点到的是菜单，终端一下都没收到。
    let open = bench.button("开终端");
    bench.cx.simulate_click(open.center(), Modifiers::default());
    bench.wait("终端", |b| {
        b.all(view::NAME)
            .iter()
            .any(|id| *id != conversation && b.bounds(*id).is_some())
    });
    let heavy = bench.all(view::NAME);
    assert!(heavy.contains(&conversation), "开终端重建了对话");
    let terminal = heavy
        .iter()
        .copied()
        .find(|id| *id != conversation)
        .unwrap();
    let menu = bench.button("菜单");
    bench.cx.simulate_click(menu.center(), Modifiers::default());
    // 菜单末尾是一个原生按钮：它在终端上摆好了，菜单才算开好；上面紧挨着的是 TSX 画的一项。
    bench.wait("菜单开在终端上", |b| {
        let (Some(id), Some(screen)) = (
            b.find(button::NAME, "label", json!("关菜单"))
                .first()
                .copied(),
            b.bounds(terminal),
        ) else {
            return false;
        };
        b.bounds(id)
            .is_some_and(|close| screen.contains(&close.center()))
    });
    let close = bench.button("关菜单");
    let item = point(close.center().x, close.top() - px(16.));
    assert!(
        bench.bounds(terminal).unwrap().contains(&item),
        "菜单项没落在终端上，测不出盖没盖住"
    );
    bench.cx.simulate_click(item, Modifiers::default());
    bench.wait("点中菜单项、菜单关上", |b| {
        !b.find(button::NAME, "label", json!("菜单：第三项"))
            .is_empty()
            && b.find(button::NAME, "label", json!("关菜单")).is_empty()
    });
    assert_eq!(bench.log.borrow().terminal_presses, 0, "点穿到终端了");
    // 点终端关得掉菜单。
    let menu = bench.button("菜单：第三项");
    bench.cx.simulate_click(menu.center(), Modifiers::default());
    bench.button("关菜单");
    let screen = bench.bounds(terminal).unwrap();
    bench.cx.simulate_click(
        point(screen.right() - px(8.), screen.bottom() - px(8.)),
        Modifiers::default(),
    );
    bench.wait("点终端关掉菜单", |b| {
        b.find(button::NAME, "label", json!("关菜单")).is_empty()
    });

    // 右键 c2 弹出 ContextMenu（同时向宿主请求 context_menu_items），点 Pin 落盘 SQLite 且挪到 c0 上方。
    let c2_id = bench.find(components::BUDDY, "seed", json!("c2"))[0];
    let c2_pos = bench.bounds(c2_id).unwrap().center();
    bench
        .cx
        .simulate_mouse_down(c2_pos, MouseButton::Right, Modifiers::default());
    bench
        .cx
        .simulate_mouse_up(c2_pos, MouseButton::Right, Modifiers::default());
    bench.wait(
        "右键菜单弹出（含 Pin 与外部插件贡献项）",
        |b| {
            !b.find(components::ICON, "path", json!("icons/pin.svg"))
                .is_empty()
                && !b
                    .find(components::ICON, "path", json!("icons/external-link.svg"))
                    .is_empty()
        },
    );
    let pin_icon = bench.find(components::ICON, "path", json!("icons/pin.svg"))[0];
    let pin_pos = bench.bounds(pin_icon).unwrap().center();
    bench.cx.simulate_click(pin_pos, Modifiers::default());
    let db_path = root
        .join("plugin-data")
        .join("session-list")
        .join("store.sqlite3");
    bench.wait("c2 置顶写入 SQLite 且挪到 c0 上方", |b| {
        let persisted = smelt_plugin_api::PluginStore::open(&db_path)
            .ok()
            .and_then(|db| {
                db.read(|conn| {
                    conn.query_row(
                        "SELECT count(*) FROM pinned_conversation WHERE conversation_id = 'c2'",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                })
                .ok()
            })
            == Some(1);
        let y0 = b
            .find(components::BUDDY, "seed", json!("c0"))
            .first()
            .and_then(|id| b.bounds(*id))
            .map(|r| r.origin.y);
        let y2 = b
            .find(components::BUDDY, "seed", json!("c2"))
            .first()
            .and_then(|id| b.bounds(*id))
            .map(|r| r.origin.y);
        persisted && matches!((y0, y2), (Some(y0), Some(y2)) if y2 < y0)
    });

    // 右键运行中的 c1 -> 点 Close -> 弹出确认框（未确认前不发 RPC）-> 点确认触发关闭 RPC。
    let c1_id = bench.find(components::BUDDY, "seed", json!("c1"))[0];
    let c1_pos = bench.bounds(c1_id).unwrap().center();
    bench
        .cx
        .simulate_mouse_down(c1_pos, MouseButton::Right, Modifiers::default());
    bench
        .cx
        .simulate_mouse_up(c1_pos, MouseButton::Right, Modifiers::default());
    bench.wait(
        "右键菜单弹出 Close 项（等外部菜单项落定）",
        |b| {
            let Some(ext_y) = b
                .find(components::ICON, "path", json!("icons/external-link.svg"))
                .first()
                .and_then(|id| b.bounds(*id))
                .map(|r| r.origin.y)
            else {
                return false;
            };
            b.find(components::ICON, "path", json!("icons/x.svg"))
                .iter()
                .any(|id| b.bounds(*id).is_some_and(|r| r.origin.y > ext_y))
        },
    );
    let ext_y = bench
        .bounds(bench.find(components::ICON, "path", json!("icons/external-link.svg"))[0])
        .unwrap()
        .origin
        .y;
    let close_icon = bench
        .find(components::ICON, "path", json!("icons/x.svg"))
        .into_iter()
        .filter(|id| bench.bounds(*id).is_some_and(|r| r.origin.y > ext_y))
        .max_by_key(|id| f32::from(bench.bounds(*id).unwrap().origin.y) as i32)
        .unwrap();
    let close_pos = bench.bounds(close_icon).unwrap().center();
    bench.cx.simulate_click(close_pos, Modifiers::default());
    let confirm_close = bench.button("Close");
    assert!(
        bench.action_log.borrow().calls.is_empty(),
        "未确认前不应发出关闭 RPC"
    );
    bench
        .cx
        .simulate_click(confirm_close.center(), Modifiers::default());
    bench.wait("确认后收到 workspace.conversation.delete RPC", |b| {
        b.action_log.borrow().calls.iter().any(|(method, params)| {
            *method == ControlMethod::WorkspaceConversationDelete
                && params["conversation_id"] == json!("c1")
        })
    });

    // 项目搜索框过滤：点搜索图标 -> 输入 beta -> 只剩 /work/beta 的 50 个小人 -> Escape 恢复 100 个。
    let search_icon = bench.find(components::ICON, "path", json!("icons/search.svg"))[0];
    let search_pos = bench.bounds(search_icon).unwrap().center();
    bench.cx.simulate_click(search_pos, Modifiers::default());
    bench.wait("进入搜索模式（搜索图标消失）", |b| {
        b.find(components::ICON, "path", json!("icons/search.svg"))
            .is_empty()
    });
    bench.cx.simulate_input("beta");
    bench.wait("按 beta 过滤后只剩 50 个小人", |b| {
        b.all(components::BUDDY).len() == HALF
            && b.find(components::BUDDY, "seed", json!("c0")).is_empty()
            && b.find(components::BUDDY, "seed", json!("c50")).len() == 1
    });
    bench.cx.simulate_keystrokes("escape");
    bench.wait("Escape 清空搜索后一百个小人回来", |b| {
        b.all(components::BUDDY).len() == PEOPLE
    });

    // 离散宿主事件转发（runtime_topic attention.changed & plugin selection-changed）。
    bench.bridge.emit_event(
        &bench.plugin_host,
        json!({
            "type": "runtime_topic",
            "machine": null,
            "topic": "attention.changed",
            "message": {
                "kind": "snapshot",
                "payload": {
                    "revision": 1,
                    "sessions": [{ "session_id": "s3", "unread": { "reason": "done" } }],
                },
            },
        }),
    );
    bench.bridge.emit_event(
        &bench.plugin_host,
        json!({
            "type": "plugin",
            "name": "smelt.sessions/selection-changed",
            "payload": {
                "kind": "conversation",
                "machine": null,
                "conversation_id": "c0",
            },
        }),
    );

    // SQLite PluginStore 持久化：折叠 /work/beta 项目（选中的 c0 在 alpha 里，beta 不受 reveal 影响），
    // 热重载 session-list 后折叠与置顶同时从 SQLite 恢复，且终端、对话、启动栏都不重建。
    let launcher_icons = bench.find(components::ICON, "path", json!("icons/panel.svg"));
    let beta_folder = bench
        .find(components::ICON, "path", json!("icons/folder.svg"))
        .into_iter()
        .max_by_key(|id| {
            bench
                .bounds(*id)
                .map_or(i32::MIN, |b| f32::from(b.origin.y) as i32)
        })
        .unwrap();
    let beta_pos = bench.bounds(beta_folder).unwrap().center();
    bench.cx.simulate_click(beta_pos, Modifiers::default());
    bench.wait("折叠 beta 后只剩 alpha 的五十个小人", |b| {
        b.all(components::BUDDY).len() == HALF
            && b.find(components::BUDDY, "seed", json!("c0")).len() == 1
            && b.find(components::BUDDY, "seed", json!("c50")).is_empty()
    });
    // 等 SQLite 异步落盘。
    bench.wait("折叠状态写入 SQLite", |_| {
        smelt_plugin_api::PluginStore::open(&db_path)
            .ok()
            .and_then(|db| {
                db.read(|conn| {
                    conn.query_row(
                        "SELECT count(*) FROM collapsed_project WHERE root = '/work/beta'",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                })
                .ok()
            })
            == Some(1)
    });
    // 热重载 session-list：VM 重启后从 SQLite 恢复折叠与置顶状态，且重组件和启动栏不动。
    let buddies_before_reload = bench.all(components::BUDDY);
    bench.edit(
        "session-list",
        "const DRAG_THRESHOLD = 4;",
        "const DRAG_THRESHOLD = 5;",
    );
    bench.wait(
        "热重载 session-list 后保持折叠与置顶且小人重新挂回",
        |b| {
            let now = b.all(components::BUDDY);
            if now.len() != HALF
                || !now.iter().all(|id| !buddies_before_reload.contains(id))
                || !b.find(components::BUDDY, "seed", json!("c50")).is_empty()
            {
                return false;
            }
            let y0 = b
                .find(components::BUDDY, "seed", json!("c0"))
                .first()
                .and_then(|id| b.bounds(*id))
                .map(|r| r.origin.y);
            let y2 = b
                .find(components::BUDDY, "seed", json!("c2"))
                .first()
                .and_then(|id| b.bounds(*id))
                .map(|r| r.origin.y);
            matches!((y0, y2), (Some(y0), Some(y2)) if y2 < y0)
        },
    );
    assert_eq!(
        bench.all(view::NAME),
        heavy,
        "热重载 session-list 重建了终端或对话"
    );
    assert_eq!(
        bench.find(components::ICON, "path", json!("icons/panel.svg")),
        launcher_icons,
        "热重载 session-list 重建了 launcher"
    );
    // 再点一次展开 /work/beta：100 个小人全部回来。
    bench.cx.simulate_click(beta_pos, Modifiers::default());
    bench.wait("展开 beta 后一百个小人回来", |b| {
        b.all(components::BUDDY).len() == PEOPLE
    });
    let people = bench.all(components::BUDDY);

    // 改 buddies：只有小人重建，session-list、launcher、终端和对话都不动。
    bench.edit("buddies", "?? \"rest\"", "?? \"wave\"");
    bench.wait("改过的小人", |b| {
        let now = b.all(components::BUDDY);
        now.len() == PEOPLE && now.iter().all(|id| !people.contains(id))
    });
    assert_eq!(bench.all(view::NAME), heavy, "改插件重建了终端或对话");
    assert_eq!(
        bench.find(components::ICON, "path", json!("icons/panel.svg")),
        launcher_icons,
        "改 buddies 重建了 launcher"
    );

    // 删掉再装回来。
    let aside = root.join("aside");
    fs::rename(bench.sources.join("buddies"), &aside).unwrap();
    bench.wait("删掉的插件撤下", |b| {
        b.all(components::BUDDY).is_empty()
    });
    fs::rename(&aside, bench.sources.join("buddies")).unwrap();
    bench.wait("装回来", |b| b.all(components::BUDDY).len() == PEOPLE);
    assert_eq!(bench.all(view::NAME), heavy, "删、装插件重建了终端或对话");
    assert_eq!(
        bench.find(components::ICON, "path", json!("icons/panel.svg")),
        launcher_icons,
        "删、装 buddies 重建了 launcher"
    );

    // 顶层抛错只坏它自己。
    bench.edit(
        "buddies",
        "const stage =",
        "throw new Error(\"boom\");\nconst stage =",
    );
    bench.wait("坏插件撤下", |b| b.all(components::BUDDY).is_empty());
    assert_eq!(bench.all(view::NAME), heavy, "插件抛错拆了终端或对话");
    assert_eq!(
        bench.find(components::ICON, "path", json!("icons/panel.svg")),
        launcher_icons,
        "插件抛错拖垮了 launcher"
    );
    bench.edit("buddies", "throw new Error(\"boom\");\n", "");
    bench.wait("改好就回来", |b| {
        b.all(components::BUDDY).len() == PEOPLE
    });

    // 装载时吃光内存（A2 强隔离）：只停掉 hungry 自己的 VM，不重启引擎，小人/列表/终端/对话/启动栏一个都不重建。
    errors.lock().unwrap().clear();
    let hungry = bench.sources.join("hungry");
    fs::create_dir_all(&hungry).unwrap();
    fs::write(
        hungry.join("main.tsx"),
        "const all = []; for (;;) all.push(new Array(1000).fill({}));\nexport default () => null;\n",
    )
    .unwrap();
    let before = bench.all(components::BUDDY);
    bench.wait("hungry 因 OOM 被单独停掉", |_| {
        errors.lock().unwrap().iter().any(|(kind, plugin)| {
            *kind == nsh_platform_gpui::AppErrorKind::Stopped && plugin.as_deref() == Some("hungry")
        })
    });
    assert_eq!(
        bench.all(components::BUDDY),
        before,
        "hungry OOM 不应重建 buddies"
    );
    assert_eq!(
        bench.all(view::NAME),
        heavy,
        "hungry OOM 不应重启引擎或拆掉终端和对话"
    );
    assert_eq!(
        bench.find(components::ICON, "path", json!("icons/panel.svg")),
        launcher_icons,
        "hungry OOM 不应影响 launcher"
    );
    bench.button("Settings");
}
