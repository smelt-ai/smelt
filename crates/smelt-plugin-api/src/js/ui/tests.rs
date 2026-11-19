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

pub(super) struct TempApp(pub(super) PathBuf);

impl TempApp {
    pub(super) fn new(source: &str) -> Self {
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
            "SmeltContextMenu",
            "SmeltMenuItem",
            "SmeltMenuSeparator",
            "SmeltMenuLabel",
            "SmeltConfirmButton",
            "SmeltCheckbox",
            "SmeltRadio",
            "SmeltTextInput",
            "SmeltAlert",
            "SmeltBadge",
            "SmeltViewInstance",
            "SmeltWindowBar",
            "SmeltSortableList",
            "SmeltListRow",
            "SmeltStickySection",
            "SmeltTruncatedLine",
            "SmeltKeybind",
            "SmeltTooltip",
            "SmeltSelect",
            "SmeltPopover",
            "SmeltSettingsPage",
            "SmeltSettingsSection",
            "SmeltSettingsRow",
            "SmeltDialog",
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
fn list_controls_build_from_scripts(cx: &mut TestAppContext) {
    let source = r#"
import { View, div } from "gpui-kit";
import {
  SmeltKeybind, SmeltListRow, SmeltSelect, SmeltStickySection, SmeltTooltip, SmeltTruncatedLine,
} from "smelt-ui";
export default class App extends View {
  init() { this.clicks = 0; this.pick = "b"; }
  render() {
    let rejected = 0;
    for (const bad of [
      () => SmeltKeybind.new("k-cmd"),
      () => SmeltKeybind.new(""),
      () => SmeltTooltip.new("", "x"),
      () => SmeltTooltip.new("t", "x").keys("k-cmd"),
      () => SmeltListRow.new(""),
    ]) {
      try { bad(); } catch (_) { rejected++; }
    }
    return div().size_full().flex().flex_col()
      .child(SmeltListRow.new("row").selected(true).h(24).w(200)
        .on_click((_e, cx) => { this.clicks++; cx.notify(); })
        .child(SmeltTruncatedLine.new("a row title")))
      .child(SmeltSelect.new("pick").w(160)
        .option("a", "Alpha").option("b", "Beta").value(this.pick)
        .on_change((value, cx) => { this.pick = value; cx.notify(); }))
      .child(SmeltTooltip.new("tip", "Say hi").keys("cmd-k cmd-s").child("hover me"))
      .child(SmeltKeybind.new("cmd-shift-k"))
      .child(SmeltStickySection.new().child("Header").child(div().child("Body")))
      .child(`state: ${this.clicks}|${this.pick}|${rejected}`);
  }
}
"#;
    let (mut context, view, _app) = mount(cx, source);
    draw(&mut context);
    let snapshot = tree(&mut context, &view);
    for text in [
        "SmeltListRow",
        "SmeltTruncatedLine",
        "SmeltSelect",
        "SmeltTooltip",
        "hover me",
        "SmeltKeybind",
        "SmeltStickySection",
        "Header",
        "Body",
        "state: 0|b|5",
    ] {
        assert!(snapshot.contains(text), "{text}: {snapshot}");
    }
    click(&mut context, 8., 8.);
    let after = tree(&mut context, &view);
    assert!(after.contains("state: 1|b|5"), "{after}");
}

#[gpui::test]
fn context_menu_reaches_a_submenu_item(cx: &mut TestAppContext) {
    let source = r#"
import { View, div } from "gpui-kit";
import { SmeltContextMenu, SmeltMenuItem, SmeltMenuLabel, SmeltMenuSeparator } from "smelt-ui";
export default class App extends View {
  init() { this.picked = "none"; }
  render() {
    const pick = (name) => (_e, cx) => { this.picked = name; cx.notify(); };
    return div().size_full()
      .child(SmeltContextMenu.new("menu").w(200).h(40)
        .items([
          SmeltMenuItem.new("Move").icon("icons/folder.svg").items([
            SmeltMenuItem.new("Here").checked(true).on_click(pick("here")),
            SmeltMenuItem.new("There").on_click(pick("there")),
          ]),
          SmeltMenuSeparator.new(),
          SmeltMenuLabel.new("Danger zone"),
          SmeltMenuItem.new("Delete").danger().disabled(true).on_click(pick("delete")),
        ])
        .child("target"))
      .child(div().absolute().top(100).child(`picked: ${this.picked}`));
  }
}
"#;
    let (mut context, view, _app) = mount(cx, source);
    context.update(|window, _| window.activate_window());
    draw(&mut context);
    context.simulate_mouse_down(
        point(px(20.), px(10.)),
        gpui::MouseButton::Right,
        Modifiers::default(),
    );
    draw(&mut context);
    assert!(
        context.debug_bounds("smelt-menu-0").is_some(),
        "menu should open"
    );
    context.simulate_keystrokes("down");
    draw(&mut context);
    context.simulate_keystrokes("right");
    draw(&mut context);
    context.simulate_keystrokes("space");
    draw(&mut context);
    let after = tree(&mut context, &view);
    assert!(after.contains("picked: here"), "{after}");
}

#[gpui::test]
fn dialog_follows_the_scripts_state(cx: &mut TestAppContext) {
    let source = r#"
import { View, div } from "gpui-kit";
import { SmeltButton, SmeltDialog } from "smelt-ui";
export default class App extends View {
  init() { this.open = true; this.log = ""; }
  render() {
    return div().size_full()
      .child(SmeltDialog.new("ask").open(this.open).title("Rename").description("Pick a name").w(300)
        .content(div().child(`body ${this.log}`))
        .footer(SmeltButton.new("ok").label("OK"))
        .on_confirm((cx) => { this.log += "confirm;"; cx.notify(); })
        .on_dismiss((cx) => { this.log += "dismiss;"; this.open = false; cx.notify(); }))
      .child(`log: ${this.log}`);
  }
}
"#;
    let (mut context, view, _app) = mount(cx, source);
    context.update(|window, _| window.activate_window());
    draw(&mut context);
    draw(&mut context);
    let is_open = |context: &mut VisualTestContext| {
        context.update(|window, cx| smelt_ui::dialog::is_open(window, cx))
    };
    assert!(is_open(&mut context), "open(true) puts a dialog up");
    context.simulate_keystrokes("enter");
    draw(&mut context);
    assert!(
        is_open(&mut context),
        "confirming leaves closing to the script"
    );
    context.simulate_keystrokes("escape");
    draw(&mut context);
    draw(&mut context);
    assert!(!is_open(&mut context), "open(false) takes it down");
    let after = tree(&mut context, &view);
    assert!(after.contains("log: confirm;dismiss;"), "{after}");
}

#[gpui::test]
fn popover_and_settings_build_from_scripts(cx: &mut TestAppContext) {
    let source = r#"
import { View, div } from "gpui-kit";
import {
  SmeltButton, SmeltPopover, SmeltSettingsPage, SmeltSettingsRow, SmeltSettingsSection,
} from "smelt-ui";
export default class App extends View {
  init() { this.open = false; this.changes = ""; }
  render() {
    return div().size_full().flex().flex_col()
      .child(SmeltPopover.new("pop").open(this.open).side("top_left").w(240)
        .on_open_change((open, cx) => { this.changes += `${open};`; this.open = open; cx.notify(); })
        .trigger(SmeltButton.new("trigger").label("Open").w(80).h(24))
        .content(div().child("inside the card")))
      .child(SmeltSettingsPage.new("settings")
        .child(SmeltSettingsSection.new().title("General").description("About the list")
          .child(SmeltSettingsRow.new("Avatar").description("What the row shows").note("Since 2026")
            .child(SmeltButton.new("x").label("X")))))
      .child(div().absolute().top(400).child(`changes: ${this.changes}`));
  }
}
"#;
    let (mut context, view, _app) = mount(cx, source);
    context.update(|window, _| window.activate_window());
    draw(&mut context);
    let snapshot = tree(&mut context, &view);
    for text in ["SmeltPopover", "SmeltSettingsPage", "SmeltSettingsRow"] {
        assert!(snapshot.contains(text), "{text}: {snapshot}");
    }
    click(&mut context, 20., 12.);
    let after = tree(&mut context, &view);
    assert!(after.contains("changes: true;"), "{after}");
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
import { SmeltConfirmButton, SmeltMenuButton, SmeltMenuItem } from "smelt-ui";
export default class App extends View {
  render() {
    let rejected = 0;
    for (const bad of [
      () => SmeltMenuItem.new(""),
      () => SmeltMenuButton.new("m").items("Open"),
      () => SmeltConfirmButton.new("c").title(1),
      () => SmeltConfirmButton.new("c").on_confirm("no"),
    ]) {
      try { bad(); } catch (_) { rejected++; }
    }
    return div().size_full()
      .child(SmeltMenuButton.new("more").label("More")
        .items([SmeltMenuItem.new("Refresh").on_click((_e, _cx) => {})])
        .absolute().left(0).top(0))
      .child(`rejected: ${rejected}`);
  }
}
"#;
    let (mut context, view, _app) = mount(cx, source);
    draw(&mut context);
    let snapshot = tree(&mut context, &view);
    assert!(snapshot.contains("rejected: 4"), "{snapshot}");
    assert!(snapshot.contains(":items(registered)"), "{snapshot}");
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

/// 顶栏跟着视图的 safe area：顶上是标题栏时占一条（下面的按钮被推下去），不是时不占地方；
/// 找不到的视图也不画。不点在条上：测试窗口不支持拖窗口。
#[gpui::test]
fn window_bar_follows_the_views_safe_area(cx: &mut TestAppContext) {
    use std::rc::Rc;

    cx.update(|cx| {
        let spec = crate::ViewSpec {
            name: "top".into(),
            title: "Top".into(),
            icon: "icons/bell.svg".into(),
            draws_top_bar: true,
            settings: None,
            instances: false,
            fills: Vec::new(),
            build: Rc::new(|_, cx| -> Rc<dyn crate::ViewHandle> {
                Rc::new(gpui::AppContext::new(cx, |_| Probe {
                    args: serde_json::Value::Null,
                }))
            }),
        };
        crate::register_views("probe", vec![spec], cx).unwrap();
    });
    let source = r#"
import { View, div } from "gpui-kit";
import { SmeltButton, SmeltWindowBar } from "smelt-ui";
export default class App extends View {
  init() { this.clicks = 0; }
  render() {
    let rejected = 0;
    for (const bad of [() => SmeltWindowBar.new(" "), () => SmeltWindowBar.new(1)]) {
      try { bad(); } catch (_) { rejected++; }
    }
    return div().size_full().flex().flex_col()
      .child(SmeltWindowBar.new("probe/top").child("bar"))
      .child(SmeltWindowBar.new("probe/missing").child("never"))
      .child(SmeltButton.new("hit").label("Hit")
        .on_click((_event, cx) => { this.clicks++; cx.notify(); }))
      .child(`rejected: ${rejected}, clicks: ${this.clicks}`);
  }
}
"#;
    let (mut context, view, _app) = mount(cx, source);
    draw(&mut context);
    assert!(tree(&mut context, &view).contains("rejected: 2, clicks: 0"));

    // 还没建、没设 safe area：两条都不占地方，按钮在最上面。
    click(&mut context, 10., 45.);
    assert!(tree(&mut context, &view).contains("clicks: 0"));
    click(&mut context, 10., 10.);
    assert!(tree(&mut context, &view).contains("clicks: 1"));

    context.update(|window, cx| {
        let (top, _) = crate::ensure_view("probe/top", window, cx).unwrap();
        let area = crate::SafeArea {
            title_bar: true,
            ..Default::default()
        };
        crate::set_safe_area(&top.view(), area, cx);
    });
    draw(&mut context);
    click(&mut context, 10., 45.);
    assert!(
        tree(&mut context, &view).contains("clicks: 2"),
        "按钮被顶栏推下去了"
    );
}

/// 按住一行拖到 `to_y` 松手。先挪一点让拖动开始，再挪到落点，落点才算上。
fn drag(context: &mut VisualTestContext, from_y: f32, to_y: f32) {
    use gpui::MouseButton;

    let (from, to) = (point(px(10.), px(from_y)), point(px(10.), px(to_y)));
    context.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
    draw(context);
    context.simulate_mouse_move(
        point(px(10.), px(from_y + 4.)),
        MouseButton::Left,
        Modifiers::default(),
    );
    draw(context);
    context.simulate_mouse_move(to, MouseButton::Left, Modifiers::default());
    draw(context);
    context.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
    draw(context);
}

/// 拖一行松在别的行上就回调挪到哪；松回原处、松在列表外不回调。
#[gpui::test]
fn sortable_list_reports_where_a_row_moved(cx: &mut TestAppContext) {
    let source = r#"
import { View, div } from "gpui-kit";
import { SmeltSortableList } from "smelt-ui";
export default class App extends View {
  init() { this.items = ["a", "b", "c"]; this.calls = []; }
  render() {
    let rejected = 0;
    for (const bad of [() => SmeltSortableList.new(""), () => SmeltSortableList.new("l").on_reorder(1)]) {
      try { bad(); } catch (_) { rejected++; }
    }
    return div().size_full().flex().flex_col()
      .child(SmeltSortableList.new("list").w(200)
        .on_reorder((from, to, cx) => {
          const [item] = this.items.splice(from, 1);
          this.items.splice(to, 0, item);
          this.calls.push(`${from}>${to}`);
          cx.notify();
        })
        .children(this.items.map((item) => div().h(30).child(`row ${item}`))))
      .child(`rejected: ${rejected}; order: ${this.items.join(",")}; calls: ${this.calls.join(" ")}`);
  }
}
"#;
    let (mut context, view, _app) = mount(cx, source);
    draw(&mut context);
    let state = |context: &mut VisualTestContext| {
        let tree = tree(context, &view);
        let start = tree.find("rejected:").expect("状态行");
        // 调试树里的文字带引号。
        let line = tree[start..].lines().next().unwrap();
        line.trim_end_matches('"').to_string()
    };
    assert!(state(&mut context).starts_with("rejected: 2; order: a,b,c; calls: "));

    // 第一行拖到第三行的下半：挪到最后。
    drag(&mut context, 15., 80.);
    assert!(
        state(&mut context).contains("order: b,c,a; calls: 0>2"),
        "{}",
        state(&mut context)
    );

    // 最后一行拖到第一行的上半：挪到最前。
    drag(&mut context, 75., 5.);
    assert!(
        state(&mut context).contains("order: a,b,c; calls: 0>2 2>0"),
        "{}",
        state(&mut context)
    );

    // 松回原处、松在列表外：不回调。
    drag(&mut context, 45., 50.);
    drag(&mut context, 45., 300.);
    assert!(
        state(&mut context).ends_with("calls: 0>2 2>0"),
        "{}",
        state(&mut context)
    );
}
