//! 插件装载与隔离关（A2 架构）：装、改、删一个插件，或者一个插件抛错 / 死循环 / OOM / 原型污染，
//! 外壳里的重组件（终端、对话）和别的插件都不动，引擎不重启。

use std::{
    cell::RefCell,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use gpui::{AnyElement, App, Entity, TestAppContext, VisualTestContext, Window};
use nsh_core::element::description::SetDescription;
use nsh_platform_gpui::{AppErrorKind, AppOptions, AppView, Limits, NativeComponent};
use serde_json::{Value, json};
use smelt_nsh::plugins::{PluginHost, Plugins};

const ROWS: usize = 100;

const SHELL: &str = r#"
import { host, nativeComponent, signal, View } from "nsh";
import { Slot } from "./slots.tsx";

const Heavy = nativeComponent<{ name: string }, {}>("test.heavy", { props: ["name"], events: [] });
const rows = Array.from({ length: 100 }, (_, index) => index);
const tag = signal("v1");
host.on("test.tag", (next: any) => tag.set(String(next)));

export function Shell() {
  return (
    <View style={{ flex: 1 }}>
      <Heavy name="terminal" style={{ width: 80, height: 20 }} />
      <Slot id="alpha" args={{ tag: tag() }} />
      <Slot id="beta" />
      {rows.map((index) => <Slot id="row" args={index} />)}
    </View>
  );
}
"#;

/// 一个插件：画一个名字里带着版本的原生组件。`top` 在模块顶层，`body` 在组件里。
fn plugin(top: &str, body: &str, shown: &str) -> String {
    format!(
        r#"import {{ nativeComponent, View }} from "nsh";
{top}
const Probe = nativeComponent<{{ name: string }}, {{}}>("test.probe", {{ props: ["name"], events: [] }});

export default function Plugin() {{
  {body}
  return <View><Probe name="{shown}" style={{{{ width: 40, height: 10 }}}} /></View>;
}}
"#
    )
}

type Log = Rc<RefCell<Vec<String>>>;
type Errors = Arc<Mutex<Vec<(AppErrorKind, Option<String>, String)>>>;
type Calls = Arc<Mutex<Vec<(String, String, Value)>>>;

/// 记下自己什么时候拿到名字、什么时候被放掉。
struct Probe {
    log: Log,
    name: String,
}

impl NativeComponent for Probe {
    fn render(&mut self, _: &mut Window, _: &mut App) -> Option<AnyElement> {
        None
    }

    fn set_prop(&mut self, _: &str, value: Option<&Value>, _: &mut Window, _: &mut App) {
        self.name = value.and_then(Value::as_str).unwrap_or_default().to_owned();
        self.log.borrow_mut().push(format!("+{}", self.name));
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.log.borrow_mut().push(format!("-{}", self.name));
    }
}

struct Bench<'a> {
    cx: &'a mut VisualTestContext,
    view: Entity<AppView>,
    plugins: Plugins,
    host: PluginHost,
    sources: PathBuf,
    log: Log,
    errors: Errors,
    calls: Calls,
    beta: u32,
}

impl Bench<'_> {
    fn write(&self, id: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = self.sources.join(id);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for (path, text) in files {
            fs::write(dir.join(path), text).unwrap();
        }
        dir
    }

    fn install(&mut self, id: &str, source: &str) {
        self.install_files(id, &[("main.tsx", source)]);
    }

    fn install_files(&mut self, id: &str, files: &[(&str, &str)]) {
        let dir = self.write(id, files);
        let change = self.plugins.install(id, &dir).unwrap();
        let host = self.host.clone();
        self.view
            .update(self.cx, |view, _| host.apply(view, change));
    }

    fn refused(&mut self, id: &str, source: &str) -> String {
        let dir = self.write(id, &[("main.tsx", source)]);
        self.plugins.install(id, &dir).expect_err("该装不上")
    }

    fn remove(&mut self, id: &str) {
        let change = self.plugins.remove(id);
        let host = self.host.clone();
        self.view
            .update(self.cx, |view, _| host.apply(view, change));
    }

    fn wait_until(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
        let start = Instant::now();
        loop {
            self.cx.run_until_parked();
            self.cx.update(|window, _| window.refresh());
            if done(self) {
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "没等到 {what}；组件 {:?}；错误 {:?}",
                self.log.borrow().iter().rev().take(12).collect::<Vec<_>>(),
                self.errors.lock().unwrap().last(),
            );
            // 轮询例外：测试平台没有「引擎线程提交了一帧」的事件可等；周期 10ms，上限 10s。
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait(&mut self, event: &str) {
        self.wait_until(event, |bench| bench.count(event) > 0);
    }

    fn count(&self, event: &str) -> usize {
        self.log.borrow().iter().filter(|e| *e == event).count()
    }

    /// beta 换一版，等它画出来：引擎还在干活，前面送进去的也都处理完了。
    fn beta_still_works(&mut self) {
        self.beta += 1;
        let shown = format!("beta{}", self.beta);
        self.install("beta", &plugin("", "", &shown));
        self.wait(&format!("+{shown}"));
    }

    /// 终端从头到尾只建过一次、没放过。
    fn terminal_kept(&self, after: &str) {
        assert_eq!(
            (self.count("+terminal"), self.count("-terminal")),
            (1, 0),
            "{after}之后终端被重建"
        );
    }
}

#[gpui::test]
fn a_plugin_only_affects_itself(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    nsh_platform_gpui::set_ui_sets(vec![nsh_base::registry()]);
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("plugins");
    let _ = fs::remove_dir_all(&root);
    let (log, errors, calls) = (Log::default(), Errors::default(), Calls::default());

    let host = PluginHost::new();
    host.set_plugin_limits(Limits {
        memory_bytes: 24 << 20,
        load_budget: Duration::from_millis(120),
        call_budget: Duration::from_millis(80),
        ..Limits::default()
    });
    for name in ["tick", "pong", "save", "proto"] {
        let calls = calls.clone();
        let method = name.to_owned();
        host.handle(name, move |plugin, call| {
            calls
                .lock()
                .unwrap()
                .push((plugin.to_owned(), method.clone(), call.data().clone()));
            call.resolve(Value::Null);
        });
    }

    let (view, cx) =
        cx.add_window_view(|window, cx| AppView::new(AppOptions::default(), window, cx));
    view.update(cx, |view, _| {
        for name in ["test.heavy", "test.probe"] {
            let log = log.clone();
            view.register_component(
                name,
                Rc::new(move |_| {
                    Box::new(Probe {
                        log: log.clone(),
                        name: String::new(),
                    })
                }),
            );
        }
        let errors = errors.clone();
        let err_host = host.clone();
        view.on_app_error(move |error| {
            errors
                .lock()
                .unwrap()
                .push((error.kind, error.plugin.clone(), error.to_string()));
            err_host.on_app_error(error);
        });
    });

    let plugins = Plugins::new(
        &root.join("stage"),
        SHELL,
        vec![SetDescription::from(nsh_base::registry())],
    )
    .unwrap();
    let first = plugins.start();
    view.update(cx, |view, _| host.apply(view, first));
    let mut bench = Bench {
        cx,
        view,
        plugins,
        host,
        sources: root.join("sources"),
        log,
        errors,
        calls,
        beta: 0,
    };
    bench.wait("+terminal");

    // 装、改、删。
    bench.install("alpha", &plugin("", "", "alpha1"));
    bench.wait("+alpha1");
    bench.beta_still_works();
    assert_eq!(bench.count("-alpha1"), 0, "装 beta 重建了 alpha");
    bench.install("alpha", &plugin("", "", "alpha2"));
    bench.wait("+alpha2");
    assert_eq!(bench.count("-beta1"), 0, "改 alpha 重建了 beta");
    bench.refused("alpha", "export default function (");
    bench.beta_still_works();
    assert_eq!(bench.count("-alpha2"), 0, "语法错的改动拆掉了上一份");
    bench.remove("alpha");
    bench.wait("-alpha2");
    bench.install("alpha", &plugin("", "", "alpha3"));
    bench.wait("+alpha3");
    bench.terminal_kept("装、改、删、再装");

    // 响应式 args 原地更新，不重建组件。
    bench.install(
        "alpha",
        r#"import { nativeComponent, View } from "nsh";
const Probe = nativeComponent<{ name: string }, {}>("test.probe", { props: ["name"], events: [] });
export default function Plugin(props: { args?: { tag?: string } }) {
  return <View><Probe name={`arg-${props.args?.tag ?? "none"}`} style={{ width: 40, height: 10 }} /></View>;
}
"#,
    );
    bench.wait("+arg-v1");
    bench.view.update(bench.cx, |view, _| {
        view.host().send("test.tag", json!("v2")).unwrap();
    });
    bench.wait("+arg-v2");
    assert_eq!(bench.count("-arg-v1"), 0, "args 更新重建了插件组件");
    assert_eq!(bench.count("-arg-v2"), 0, "args 更新卸载了插件组件");

    // 出不了自己的目录，也只能用宿主给的模块。
    bench.refused(
        "alpha",
        &plugin(
            "import Other from '../../beta/plugin/main.tsx';\nvoid Other;",
            "",
            "x",
        ),
    );
    bench.refused(
        "alpha",
        &plugin("import * as m from 'nope';\nvoid m;", "", "x"),
    );

    // 同插件另一个文件里原先没人用的导出，改动里新用上。
    let shared = "export const one = () => 1;\nexport const two = () => 2;\n";
    let uses = "import { one } from './m.ts';\nimport { c } from './c.ts';\none(); c();";
    bench.install_files(
        "alpha",
        &[
            ("main.tsx", &plugin(uses, "", "files1")),
            ("m.ts", shared),
            ("c.ts", "export const c = () => 0;\n"),
        ],
    );
    bench.wait("+files1");
    bench.install_files(
        "alpha",
        &[
            ("main.tsx", &plugin(uses, "", "files2")),
            ("m.ts", shared),
            (
                "c.ts",
                "import { two } from './m.ts';\nexport const c = () => two();\n",
            ),
        ],
    );
    bench.wait("+files2");

    // 第一次用到一个宿主模块，不重起引擎。
    for (n, module) in ["nsh/base", "nsh/desktop", "nsh/i18n"].iter().enumerate() {
        let shown = format!("module{n}");
        bench.install(
            "alpha",
            &plugin(
                &format!("import * as m from '{module}';\nvoid m;"),
                "",
                &shown,
            ),
        );
        bench.wait(&format!("+{shown}"));
    }
    bench.terminal_kept("用到新的宿主模块");

    // 坏插件只坏它自己（含顶层抛错、组件抛错、顶层/组件/定时器死循环）。
    for (what, top, body) in [
        ("顶层抛错", "throw new Error('top');", ""),
        ("组件里抛错", "", "throw new Error('body');"),
        ("顶层死循环", "for (;;) {}", ""),
        ("组件里死循环", "", "for (;;) {}"),
        (
            "定时器里死循环",
            "setTimeout(() => { for (;;) {} }, 1);",
            "",
        ),
    ] {
        bench.install("alpha", &plugin(top, body, "broken"));
        bench.beta_still_works();
        bench.terminal_kept(what);
    }

    // 卸掉或换掉之后，它顶层起的定时器和监听都停。
    let ticking = "import { host } from 'nsh';\n\
                   setInterval(() => host.post('tick', 1), 20);\n\
                   host.on('test.ping', () => host.post('pong', 1));";
    for replace in [false, true] {
        bench.calls.lock().unwrap().clear();
        bench.install("alpha", &plugin(ticking, "", "ticking"));
        bench.wait_until("定时器在跑", |b| !b.calls.lock().unwrap().is_empty());
        if replace {
            bench.install("alpha", &plugin("", "", "quiet"));
        } else {
            bench.remove("alpha");
        }
        bench.beta_still_works();
        bench.calls.lock().unwrap().clear();
        bench.host.broadcast("test.ping", Value::Null);
        // 轮询例外：要证明的是「不再发生」，没有事件可等；等够十个定时器周期。
        std::thread::sleep(Duration::from_millis(200));
        bench.beta_still_works();
        let late = bench.calls.lock().unwrap().clone();
        assert_eq!(late, Vec::new(), "卸掉的插件还在跑");
    }

    // 宿主知道是谁调的，插件自己填不了。
    let calling = "import { host } from 'nsh';\nhost.post('save', { plugin: 'beta' });";
    bench.install("alpha", &plugin(calling, "", "calling"));
    bench.wait("+calling");
    let call = bench.calls.lock().unwrap().last().cloned().unwrap();
    assert_eq!((call.0.as_str(), call.1.as_str()), ("alpha", "save"));

    // harden: true 原型冻结与跨 VM 堆隔离。
    bench.calls.lock().unwrap().clear();
    bench.install(
        "alpha",
        &plugin(
            "import { host } from 'nsh';\n\
             let blocked = false;\n\
             try { (Object.prototype as any).evil = 1; } catch { blocked = true; }\n\
             host.post('proto', { blocked, seen: ({} as any).evil ?? null });",
            "",
            "proto-a",
        ),
    );
    bench.wait("+proto-a");
    bench.wait_until("proto 上报", |b| !b.calls.lock().unwrap().is_empty());
    let proto_a = bench.calls.lock().unwrap().last().cloned().unwrap();
    assert_eq!(proto_a.2["blocked"], json!(true));
    assert_eq!(proto_a.2["seen"], Value::Null);

    // 禁止插件用 smelt/slots 嵌套其它插件：打包校验直接拒绝，只有宿主外壳拥有挂载插件的权限。
    bench.refused(
        "alpha",
        r#"import { View } from "nsh";
import { Slot } from "smelt/slots";
export default function BadNest() {
  return <View><Slot id="beta" /></View>;
}
"#,
    );
    bench.refused(
        "alpha",
        r#"import { View } from "nsh";
import { Slot } from "../slots.tsx";
export default function BadRelativeNest() {
  return <View><Slot id="beta" /></View>;
}
"#,
    );

    // 一个视图摆很多行：同一个独立 VM，一百个插槽一次重放。
    bench.install(
        "row",
        r#"import { nativeComponent, View } from "nsh";
const Probe = nativeComponent<{ name: string }, {}>("test.probe", { props: ["name"], events: [] });
export default function Row(props: { args: number }) {
  return <View><Probe name={`row${props.args}`} style={{ width: 40, height: 2 }} /></View>;
}
"#,
    );
    bench.wait_until("一百行", |b| {
        (0..ROWS).all(|n| b.count(&format!("+row{n}")) == 1)
    });
    bench.terminal_kept("一百行");

    // 内存耗尽（A2 强隔离）：只停掉 alpha 自己的 VM，不重启引擎，终端和 beta 丝毫不受影响。
    bench.errors.lock().unwrap().clear();
    let hungry = "const all = []; for (;;) all.push(new Array(1000).fill({}));";
    bench.install("alpha", &plugin(hungry, "", "hungry"));
    bench.wait_until("alpha OOM 熔断", |b| {
        b.errors.lock().unwrap().iter().any(|(kind, plugin, _)| {
            *kind == AppErrorKind::Stopped && plugin.as_deref() == Some("alpha")
        })
    });
    bench.terminal_kept("内存耗尽");
    bench.beta_still_works();

    // 冷启动时装着一个顶层死循环的：外壳和别的插件照常起来，也精确报出停在 alpha。
    bench.install("alpha", &plugin("for (;;) {}", "", "broken"));
    bench.beta_still_works();
    bench.log.borrow_mut().clear();
    bench.errors.lock().unwrap().clear();
    let restart = bench.plugins.start();
    let host = bench.host.clone();
    bench
        .view
        .update(bench.cx, |view, _| host.apply(view, restart));
    bench.wait("+terminal");
    bench.wait(&format!("+beta{}", bench.beta));
    bench.wait_until("冷启动报出 alpha 停掉", |b| {
        b.errors.lock().unwrap().iter().any(|(kind, plugin, _)| {
            *kind == AppErrorKind::Stopped && plugin.as_deref() == Some("alpha")
        })
    });
}
