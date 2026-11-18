use std::{
    fs,
    ops::Deref,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, point, px, size};
use gpui_shell::{
    ArgumentSchema, ComponentRegistry, FrozenComponentRegistry, ScriptView, ShellRuntime,
};

static NEXT_APP: AtomicU64 = AtomicU64::new(0);

pub(super) struct TempApp(PathBuf);

impl TempApp {
    fn new(source: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "smelt-shell-ui-{}-{}",
            std::process::id(),
            NEXT_APP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("main.js"), source).unwrap();
        Self(path)
    }
}

impl Drop for TempApp {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

/// 用插件运行时的那份组件目录挂一个脚本。
pub(super) fn mount(
    cx: &mut TestAppContext,
    source: &str,
) -> (VisualTestContext, Entity<ScriptView>, TempApp) {
    mount_frozen(cx, source, super::components().unwrap())
}

pub(super) fn mount_with_registry(
    cx: &mut TestAppContext,
    source: &str,
    registry: ComponentRegistry,
) -> (VisualTestContext, Entity<ScriptView>, TempApp) {
    mount_frozen(cx, source, registry.freeze().unwrap())
}

fn mount_frozen(
    cx: &mut TestAppContext,
    source: &str,
    components: FrozenComponentRegistry,
) -> (VisualTestContext, Entity<ScriptView>, TempApp) {
    // 和应用启动一样：Shell 机制（含 gpui-base）加 smelt-ui 的全局件；关掉动画，弹层
    // 一帧就到位。
    cx.update(|cx| {
        gpui_shell::init(cx);
        smelt_ui::init(cx);
        cx.set_reduce_motion(true);
    });
    let app = TempApp::new(source);
    let runtime = ShellRuntime::new_isolated_with_components(components).unwrap();
    let loaded = runtime.load_application(&app.0, "main.js").unwrap();
    let mounted = std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot = mounted.clone();
    let window = cx.open_window(size(px(500.), px(480.)), move |window, cx| {
        let view = runtime.mount_application(&loaded, window, cx).unwrap();
        *slot.borrow_mut() = Some(view.clone());
        gpui_base::Root::new(view, window, cx)
    });
    let context = VisualTestContext::from_window(*window.deref(), cx);
    let view = mounted.borrow().clone().unwrap();
    (context, view, app)
}

pub(super) fn draw(context: &mut VisualTestContext) {
    context.run_until_parked();
    context.update(|window, cx| window.draw(cx).clear(cx));
}

pub(super) fn tree(context: &mut VisualTestContext, view: &Entity<ScriptView>) -> String {
    context.update(|_, cx| view.read(cx).snapshot().unwrap().debug_tree())
}

fn click(context: &mut VisualTestContext, x: f32, y: f32) {
    context.simulate_click(point(px(x), px(y)), Modifiers::default());
    draw(context);
}

#[test]
fn components_hold_only_smelt_controls() {
    let components = super::components().unwrap();
    assert_eq!(components.module_specifier(), Some(super::MODULE));
    let names = components
        .descriptors()
        .map(|descriptor| descriptor.name())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "SmeltButton",
            "SmeltMenuButton",
            "SmeltConfirmButton",
            "SmeltCheckbox",
            "SmeltRadio",
            "SmeltTextInput",
            "SmeltAlert",
            "SmeltBadge",
            "SmeltViewInstance",
        ]
    );
    let states = components
        .states()
        .map(|state| state.export())
        .collect::<Vec<_>>();
    assert_eq!(states, ["SmeltInputState"]);
    assert!(components.states().all(|state| state.has_input_events()));
    assert!(
        components.initializer().is_none(),
        "smelt_ui::init 由应用启动时调"
    );
    assert!(components.descriptors().all(|descriptor| {
        descriptor.documentation().is_some()
            && descriptor
                .methods()
                .iter()
                .all(|method| method.documentation().is_some())
    }));
    let input = components
        .descriptors()
        .find(|descriptor| descriptor.name() == "SmeltTextInput")
        .unwrap();
    assert_eq!(
        input.constructors()[0].arguments()[0].schema(),
        &ArgumentSchema::Entity("SmeltInputState")
    );
}

#[gpui::test]
fn choices_report_through_their_callbacks(cx: &mut TestAppContext) {
    let source = r#"
import { View, div } from "gpui-kit";
import { SmeltCheckbox, SmeltRadio } from "smelt-ui";
export default class App extends View {
  init() { this.on = false; this.pick = "a"; }
  render() {
    return div().size_full()
      .child(SmeltCheckbox.new("box").label("Box").checked(this.on)
        .absolute().left(0).top(0)
        .on_change((checked, cx) => { this.on = checked; cx.notify(); }))
      .child(SmeltRadio.new("a").label("A").checked(this.pick === "a")
        .absolute().left(0).top(40)
        .on_select((cx) => { this.pick = "a"; cx.notify(); }))
      .child(SmeltRadio.new("b").label("B").checked(this.pick === "b")
        .absolute().left(0).top(80)
        .on_select((cx) => { this.pick = "b"; cx.notify(); }))
      .child(SmeltRadio.new("c").label("C").checked(this.pick === "c").disabled(true)
        .absolute().left(0).top(120)
        .on_select((cx) => { this.pick = "c"; cx.notify(); }))
      .child(`state: ${this.on}|${this.pick}`);
  }
}
"#;
    let (mut context, view, _app) = mount(cx, source);
    draw(&mut context);
    assert!(tree(&mut context, &view).contains("state: false|a"));
    click(&mut context, 8., 8.);
    click(&mut context, 8., 88.);
    let after = tree(&mut context, &view);
    assert!(after.contains("state: true|b"), "{after}");
    click(&mut context, 8., 8.);
    click(&mut context, 8., 128.);
    let again = tree(&mut context, &view);
    assert!(
        again.contains("state: false|b"),
        "disabled must not select: {again}"
    );
}

#[gpui::test]
fn text_input_state_reports_changes(cx: &mut TestAppContext) {
    let source = r#"
import { View, div } from "gpui-kit";
import { SmeltInputState, SmeltTextInput } from "smelt-ui";
export default class App extends View {
  init(_props, cx) {
    this.query = "";
    this.search = SmeltInputState.new("Search");
    this.search.on("change", (_event, cx) => { this.query = this.search.value(); cx.notify(); });
  }
  render() {
    return div().size_full()
      .child(SmeltTextInput.new(this.search).aria_label("Search").small().cleanable(true)
        .absolute().left(0).top(0).w(200))
      .child(`query: [${this.query}]`);
  }
}
"#;
    let (mut context, view, _app) = mount(cx, source);
    draw(&mut context);
    click(&mut context, 40., 12.);
    context.simulate_input("abc");
    draw(&mut context);
    let typed = tree(&mut context, &view);
    assert!(typed.contains("query: [abc]"), "{typed}");
}

#[gpui::test]
fn badge_and_alert_build_from_scripts(cx: &mut TestAppContext) {
    let source = r#"
import { View, div } from "gpui-kit";
import { SmeltAlert, SmeltBadge } from "smelt-ui";
export default class App extends View {
  render() {
    let rejected = 0;
    for (const bad of [
      () => SmeltAlert.new("", "x"),
      () => SmeltAlert.new("a"),
    ]) {
      try { bad(); } catch (_) { rejected++; }
    }
    return div().size_full().flex().flex_col()
      .child(SmeltBadge.new().need().child("needs you"))
      .child(SmeltBadge.new().child("neutral"))
      .child(SmeltAlert.new("warn", "will replace").need().w(300))
      .child(SmeltAlert.new("error", "failed"))
      .child(`rejected: ${rejected}`);
  }
}
"#;
    let (mut context, view, _app) = mount(cx, source);
    draw(&mut context);
    let snapshot = tree(&mut context, &view);
    for text in [
        "SmeltBadge",
        "needs you",
        "neutral",
        "SmeltAlert",
        ":need(registered)",
        "rejected: 2",
    ] {
        assert!(snapshot.contains(text), "{text}: {snapshot}");
    }
}

#[gpui::test]
fn confirm_button_runs_only_after_confirming(cx: &mut TestAppContext) {
    let source = r#"
import { View, div } from "gpui-kit";
import { SmeltConfirmButton } from "smelt-ui";
export default class App extends View {
  init() { this.done = 0; }
  render() {
    return div().size_full()
      .child(SmeltConfirmButton.new("trash").label("Delete…").small()
        .title("Delete it?").description("It moves to the trash.").action("Delete")
        .on_confirm((cx) => { this.done++; cx.notify(); })
        .absolute().left(0).top(0))
      .child(`done: ${this.done}`);
  }
}
"#;
    let (mut context, view, _app) = mount(cx, source);
    draw(&mut context);
    click(&mut context, 10., 12.);
    assert!(context.update(|window, cx| smelt_ui::dialog::is_open(window, cx)));
    assert!(
        tree(&mut context, &view).contains("done: 0"),
        "opening must not run it"
    );

    context.simulate_keystrokes("escape");
    draw(&mut context);
    assert!(!context.update(|window, cx| smelt_ui::dialog::is_open(window, cx)));
    assert!(
        tree(&mut context, &view).contains("done: 0"),
        "cancel must not run it"
    );

    click(&mut context, 10., 12.);
    context.simulate_keystrokes("enter");
    draw(&mut context);
    assert!(!context.update(|window, cx| smelt_ui::dialog::is_open(window, cx)));
    let confirmed = tree(&mut context, &view);
    assert!(confirmed.contains("done: 1"), "{confirmed}");
}

#[gpui::test]
fn confirm_and_menu_reject_incomplete_descriptions(cx: &mut TestAppContext) {
    let source = r#"
import { View, div } from "gpui-kit";
import { SmeltConfirmButton, SmeltMenuButton } from "smelt-ui";
export default class App extends View {
  render() {
    let rejected = 0;
    for (const bad of [
      () => SmeltMenuButton.new("m").item("", (_cx) => {}),
      () => SmeltMenuButton.new("m").item("Open"),
      () => SmeltConfirmButton.new("c").title(1),
      () => SmeltConfirmButton.new("c").on_confirm("no"),
    ]) {
      try { bad(); } catch (_) { rejected++; }
    }
    return div().size_full()
      .child(SmeltMenuButton.new("more").label("More").item("Refresh", (_cx) => {})
        .absolute().left(0).top(0))
      .child(`rejected: ${rejected}`);
  }
}
"#;
    let (mut context, view, _app) = mount(cx, source);
    draw(&mut context);
    let snapshot = tree(&mut context, &view);
    assert!(snapshot.contains("rejected: 4"), "{snapshot}");
    assert!(snapshot.contains(":item(registered)"), "{snapshot}");
}

/// 按实例建的视图，记下打开它的参数。
struct Probe {
    args: serde_json::Value,
}

impl gpui::Render for Probe {
    fn render(
        &mut self,
        _: &mut gpui::Window,
        _: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        gpui::div()
    }
}

impl crate::PluginView for Probe {
    fn open(
        &mut self,
        args: &serde_json::Value,
        _: &mut gpui::Window,
        _: &mut gpui::Context<Self>,
    ) {
        self.args = args.clone();
    }
}

/// 实例跟着元素走：连着画是同一份，换参数换一份，不画了就放掉；建在绘制之后，不在 render 里。
#[gpui::test]
fn view_instance_follows_its_element(cx: &mut TestAppContext) {
    use std::{cell::RefCell, rc::Rc};

    let built: Rc<RefCell<Vec<gpui::WeakEntity<Probe>>>> = Rc::default();
    let builder = built.clone();
    cx.update(|cx| {
        let spec = crate::ViewSpec {
            name: "pane".into(),
            title: "Pane".into(),
            icon: "icons/bell.svg".into(),
            draws_top_bar: false,
            settings: None,
            instances: true,
            fills: Vec::new(),
            build: Rc::new(move |_, cx| -> Rc<dyn crate::ViewHandle> {
                let probe = gpui::AppContext::new(cx, |_| Probe {
                    args: serde_json::Value::Null,
                });
                builder.borrow_mut().push(probe.downgrade());
                Rc::new(probe)
            }),
        };
        crate::register_views("probe", vec![spec], cx).unwrap();
    });
    let source = r#"
import { View, div } from "gpui-kit";
import { SmeltButton, SmeltViewInstance } from "smelt-ui";
export default class App extends View {
  init() { this.session = "a"; this.shown = true; }
  render() {
    let rejected = 0;
    for (const bad of [
      () => SmeltViewInstance.new(" "),
      () => SmeltViewInstance.new("probe/pane").args("{"),
    ]) {
      try { bad(); } catch (_) { rejected++; }
    }
    const root = div().size_full()
      .child(SmeltButton.new("next").label("Next").absolute().left(0).top(0)
        .on_click((_event, cx) => { this.session = "b"; cx.notify(); }))
      .child(SmeltButton.new("hide").label("Hide").absolute().left(0).top(40)
        .on_click((_event, cx) => { this.shown = false; cx.notify(); }))
      .child(`rejected: ${rejected}`);
    return this.shown
      ? root.child(SmeltViewInstance.new("probe/pane")
          .args(JSON.stringify({ session: this.session }))
          .absolute().left(0).top(80).w(200).h(100))
      : root;
  }
}
"#;
    let (mut context, view, _app) = mount(cx, source);
    let live = |context: &mut VisualTestContext| {
        context.update(|_, cx| {
            built
                .borrow()
                .iter()
                .filter_map(|probe| probe.upgrade())
                .map(|probe| probe.read(cx).args.clone())
                .collect::<Vec<_>>()
        })
    };
    draw(&mut context);
    draw(&mut context);
    assert!(tree(&mut context, &view).contains("rejected: 2"));
    assert_eq!(live(&mut context), [serde_json::json!({ "session": "a" })]);

    draw(&mut context);
    assert_eq!(built.borrow().len(), 1, "连着画不重建");

    click(&mut context, 10., 12.);
    draw(&mut context);
    assert_eq!(live(&mut context), [serde_json::json!({ "session": "b" })]);
    assert_eq!(built.borrow().len(), 2);

    click(&mut context, 10., 52.);
    draw(&mut context);
    assert!(live(&mut context).is_empty(), "不画了就放掉");
}
