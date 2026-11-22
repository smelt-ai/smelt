//! 试点这一整套：真外壳、`pilot-plugins/` 里真的插件、真的原生组件、真的文件监视，在测试平台的
//! 窗口里跑。终端换成一块记按下次数的假视图；宿主状态直接塞，不建假宿主。

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

use super::*;

const PEOPLE: usize = 100;

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

struct Bench<'a> {
    cx: &'a mut VisualTestContext,
    _app: Entity<AppView>,
    log: Rc<RefCell<Log>>,
    sources: PathBuf,
}

impl Bench<'_> {
    fn wait(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
        let start = Instant::now();
        loop {
            self.cx.run_until_parked();
            self.cx.update(|window, _| window.refresh());
            self.cx.run_until_parked();
            if done(self) {
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "没等到{what}；建着的：{:?}",
                self.kinds()
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

fn state(running: &str) -> Value {
    let people: Vec<String> = (0..PEOPLE).map(|n| format!("c{n}")).collect();
    json!({
        "locale": "en",
        "active_context": { "session": null },
        "machines": [{
            "machine": null,
            "directory": { "conversations": people
                .iter()
                .map(|id| json!({ "id": id, "terminals": [] }))
                .collect::<Vec<_>>() },
            "facts": people
                .iter()
                .map(|id| json!({ "id": id, "status": if id == running { "running" } else { "idle" } }))
                .collect::<Vec<_>>(),
        }],
        "pages": [],
        "views": [{
            "id": "test/panel", "title": "Panel", "icon": "icons/panel.svg", "settings": null,
            "instances": false, "badge": null, "laid_out": false, "shown": false,
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
            "palette": { "text": "#eeeeee", "muted": "#888888", "muted_selected": "#aaaaaa", "bad": "#ff0000" },
            "type": {
                "ui": { "size": 13, "line_height": 20, "weight": 400 },
                "ui_medium": { "size": 13, "line_height": 20, "weight": 500 },
            },
            "radius": { "row": 8 },
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
    module::set_for_test(
        (0..PEOPLE)
            .map(|n| module::Conversation {
                id: format!("s{n}"),
                conversation: format!("c{n}"),
                title: format!("Chat {n}"),
            })
            .collect(),
    );

    let log = Rc::new(RefCell::new(Log::default()));
    let (app, cx) =
        cx.add_window_view(|window, cx| AppView::new(AppOptions::default(), window, cx));
    // 原生组件只建看得见的那些：窗口够高，一百行才都在。
    cx.simulate_resize(size(px(1000.), px(5600.)));
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
    let errors: std::sync::Arc<
        std::sync::Mutex<Vec<(nsh_platform_gpui::AppErrorKind, Option<String>)>>,
    > = std::sync::Arc::default();
    let plugin_host = PluginHost::new();
    plugin_host.set_plugin_limits(Limits {
        memory_bytes: 64 << 20,
        ..Limits::default()
    });
    let bridge = host::Bridge::default();
    let mut runs = bridge.serve(&plugin_host);
    bridge.push(&plugin_host, state("c0"));
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
    };

    // 一百行一百个小人，状态是宿主给的那份。
    bench.wait("一百个小人", |b| {
        b.all(components::BUDDY).len() == PEOPLE
            && b.find(components::BUDDY, "seed", json!("c0")).len() == 1
            && !b.all(components::ICON).is_empty()
    });
    let first = bench.find(components::BUDDY, "seed", json!("c0"))[0];
    let act = |b: &Bench, id: usize| b.log.borrow().live.get(&id)?.props.get("act").cloned();
    bench.wait("c0 在干活", |b| {
        act(b, first).is_some_and(|act| act != "rest")
    });
    let people = bench.all(components::BUDDY);
    bridge.push(&plugin_host, state("c1"));
    let second = bench.find(components::BUDDY, "seed", json!("c1"))[0];
    bench.wait("状态变了跟着演", |b| {
        act(b, second).is_some_and(|act| act != "rest")
    });
    assert_eq!(bench.all(components::BUDDY), people, "状态一变重建了小人");

    // 文案按语言取，按钮点下去宿主收到命令。
    let settings = bench.button("Settings");
    bench
        .cx
        .simulate_click(settings.center(), Modifiers::default());
    let sent = RefCell::new(None);
    let received = RefCell::new(&mut runs);
    bench.wait("设置那条命令", |_| {
        if let Ok(run) = received.borrow_mut().try_recv() {
            sent.replace(Some(run.command.clone()));
            run.call.resolve(Value::Null);
        }
        sent.borrow().is_some()
    });
    assert_eq!(sent.into_inner().as_deref(), Some("smelt.app/settings"));

    // 行里的小人、图标只是画：点在它们上面也是点这一行。
    let icon = bench.all(components::ICON)[0];
    let at = bench.bounds(icon).unwrap().center();
    bench.cx.simulate_click(at, Modifiers::default());
    let sent = RefCell::new(None);
    bench.wait("点图标打开那一行", |_| {
        if let Ok(run) = received.borrow_mut().try_recv() {
            sent.replace(Some((run.command.clone(), run.args.clone())));
            run.call.resolve(Value::Null);
        }
        sent.borrow().is_some()
    });
    assert_eq!(
        sent.into_inner(),
        Some((
            "smelt.app/open-view".to_owned(),
            json!({ "view": "test/panel" })
        ))
    );
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

    // 改一个插件：只有它那几格重建。
    let icons = bench.all(components::ICON);
    bench.edit("buddies", "?? \"rest\"", "?? \"wave\"");
    bench.wait("改过的小人", |b| {
        let now = b.all(components::BUDDY);
        now.len() == PEOPLE && now.iter().all(|id| !people.contains(id))
    });
    assert_eq!(bench.all(view::NAME), heavy, "改插件重建了终端或对话");
    assert_eq!(
        bench.all(components::ICON),
        icons,
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
        bench.all(components::ICON),
        icons,
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
        bench.all(components::ICON),
        icons,
        "插件抛错拖垮了 launcher"
    );
    bench.edit("buddies", "throw new Error(\"boom\");\n", "");
    bench.wait("改好就回来", |b| {
        b.all(components::BUDDY).len() == PEOPLE
    });

    // 装载时吃光内存（A2 强隔离）：只停掉 hungry 自己的 VM，不重启引擎，小人/终端/对话/启动栏一个都不重建。
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
        bench.all(components::ICON),
        icons,
        "hungry OOM 不应影响 launcher"
    );
    bench.button("Settings");
}
