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
            "SmeltScrollView",
            "SmeltScrollAnchor",
            "SmeltDragItem",
            "SmeltFold",
            "SmeltFoldChevron",
            "SmeltFlip",
            "SmeltArrival",
            "SmeltWash",
            "SmeltGlowIcon",
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

/// 改名：一出来就拿到焦点、原名全选，直接打字就换掉。
#[gpui::test]
fn text_input_state_takes_focus_and_selects_all(cx: &mut TestAppContext) {
    let source = r#"
import { View, div } from "gpui-kit";
import { SmeltInputState, SmeltTextInput } from "smelt-ui";
export default class App extends View {
  init(_props, cx) {
    this.name = SmeltInputState.new();
    this.name.on("change", (_event, cx) => cx.notify());
  }
  render(cx) {
    return div().size_full()
      .child(div().id("rename").absolute().left(0).top(0).w(100).h(20)
        .on_click(() => { this.name.set_value("old name"); this.name.focus(); this.name.select_all(); }))
      .child(SmeltTextInput.new(this.name).aria_label("Name").absolute().left(0).top(40).w(200))
      .child(`name: [${this.name.value()}]`);
  }
}
"#;
    let (mut context, view, _app) = mount(cx, source);
    draw(&mut context);
    click(&mut context, 20., 10.);
    draw(&mut context);
    context.simulate_input("new");
    draw(&mut context);
    let typed = tree(&mut context, &view);
    assert!(typed.contains("name: [new]"), "{typed}");
}

/// Esc：框里没什么要先收掉的，交给脚本（取消改名、收起搜索）。
#[gpui::test]
fn text_input_reports_escape(cx: &mut TestAppContext) {
    let source = r#"
import { View, div } from "gpui-kit";
import { SmeltInputState, SmeltTextInput } from "smelt-ui";
export default class App extends View {
  init() { this.name = SmeltInputState.new(); this.escaped = 0; }
  render() {
    return div().size_full()
      .child(SmeltTextInput.new(this.name).aria_label("Name").absolute().left(0).top(0).w(200)
        .on_escape((cx) => { this.escaped++; cx.notify(); }))
      .child(`escaped: ${this.escaped}`);
  }
}
"#;
    let (mut context, view, _app) = mount(cx, source);
    draw(&mut context);
    click(&mut context, 40., 12.);
    context.simulate_input("abc");
    context.simulate_keystrokes("escape");
    draw(&mut context);
    let escaped = tree(&mut context, &view);
    assert!(escaped.contains("escaped: 1"), "{escaped}");
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

/// 菜单贴哪个角原样交给 `MenuTrigger::anchor`；认不得的角拒掉。测试窗口里量不到触发器的位置
/// （弹层贴到窗口边上），这里不比坐标。
#[gpui::test]
fn menu_button_takes_an_anchor(cx: &mut TestAppContext) {
    let source = r#"
import { View, div } from "gpui-kit";
import { SmeltMenuButton, SmeltMenuItem } from "smelt-ui";
export default class App extends View {
  render() {
    let rejected = 0;
    try { SmeltMenuButton.new("bad").anchor("middle"); } catch (_) { rejected++; }
    return div().size_full()
      .child(SmeltMenuButton.new("more").label("More").anchor("top_right")
        .items([SmeltMenuItem.new("Refresh").on_click((_e, _cx) => {})])
        .absolute().left(300).top(0))
      .child(div().absolute().top(100).child(`rejected: ${rejected}`));
  }
}
"#;
    let (mut context, view, _app) = mount(cx, source);
    context.update(|window, _| window.activate_window());
    draw(&mut context);
    assert!(tree(&mut context, &view).contains("rejected: 1"));
    click(&mut context, 320., 10.);
    assert!(
        context.debug_bounds("smelt-menu-0").is_some(),
        "menu should open"
    );
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

/// 别处关掉了（全局的 `dialog::close`）：脚本经 on_dismiss 知道，再要开时还开得出来。
#[gpui::test]
fn dialog_closed_elsewhere_tells_the_script(cx: &mut TestAppContext) {
    let source = r#"
import { View, div } from "gpui-kit";
import { SmeltButton, SmeltDialog } from "smelt-ui";
export default class App extends View {
  init() { this.open = true; this.log = ""; }
  render() {
    return div().size_full()
      .child(SmeltButton.new("reopen").label("Reopen").h(20)
        .on_click((_, cx) => { this.open = true; cx.notify(); }))
      .child(SmeltDialog.new("ask").open(this.open).title("Rename")
        .on_dismiss((cx) => { this.log += "dismiss;"; this.open = false; cx.notify(); }))
      .child(`log: ${this.log}`);
  }
}
"#;
    let (mut context, view, _app) = mount(cx, source);
    draw(&mut context);
    draw(&mut context);
    let is_open = |context: &mut VisualTestContext| {
        context.update(|window, cx| smelt_ui::dialog::is_open(window, cx))
    };
    assert!(is_open(&mut context));
    context.update(|window, cx| smelt_ui::dialog::close(window, cx));
    draw(&mut context);
    draw(&mut context);
    assert!(!is_open(&mut context));
    let after = tree(&mut context, &view);
    assert!(after.contains("log: dismiss;"), "{after}");
    click(&mut context, 10., 10.);
    draw(&mut context);
    assert!(
        is_open(&mut context),
        "reopens after being closed elsewhere"
    );
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
fn scroll_view_reveals_and_reports_offscreen_rows(cx: &mut TestAppContext) {
    let source = r#"
import { View, div } from "gpui-kit";
import { SmeltButton, SmeltScrollAnchor, SmeltScrollView } from "smelt-ui";
export default class App extends View {
  init() { this.serial = 0; this.sides = "none"; }
  render() {
    const rows = [];
    for (let i = 0; i < 20; i++) rows.push(SmeltScrollAnchor.new(`r${i}`).h(30).child(`row ${i}`));
    return div().size_full().flex().flex_col()
      .child(SmeltButton.new("go").label("Go").h(20)
        .on_click((_, cx) => { this.serial += 1; cx.notify(); }))
      .child((this.serial > 0 ? SmeltScrollView.new("list").reveal("r19", this.serial) : SmeltScrollView.new("list"))
        .h(100).edges().watch(["r0", "r19"])
        .on_offscreen((above, below, cx) => { this.sides = `${above.join(",")}|${below.join(",")}`; cx.notify(); })
        .overlay_top(div().child("pill"))
        .children(rows))
      .child(div().child(`sides: ${this.sides}`));
  }
}
"#;
    let (mut context, view, _app) = mount(cx, source);
    draw(&mut context);
    draw(&mut context);
    draw(&mut context);
    let before = tree(&mut context, &view);
    assert!(before.contains("sides: |r19"), "{before}");
    click(&mut context, 10., 10.);
    draw(&mut context);
    draw(&mut context);
    draw(&mut context);
    let after = tree(&mut context, &view);
    assert!(after.contains("sides: r0|"), "{after}");

    // 滚轮滚回顶上：滚动重画脚本视图，两边跟着报。
    context.simulate_event(gpui::ScrollEvent {
        position: point(px(10.), px(60.)),
        delta: gpui::ScrollDelta::Pixels(point(px(0.), px(1000.))),
        ..Default::default()
    });
    draw(&mut context);
    draw(&mut context);
    let back = tree(&mut context, &view);
    assert!(back.contains("sides: |r19"), "{back}");
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
fn drag_items_report_drops_of_their_own_kind(cx: &mut TestAppContext) {
    let source = r#"
import { View, div } from "gpui-kit";
import { SmeltDragItem, SmeltScrollView } from "smelt-ui";
export default class App extends View {
  init() { this.items = ["a", "b", "c"]; this.calls = []; this.files = ""; }
  render() {
    let rejected = 0;
    for (const bad of [() => SmeltDragItem.new("", "row"), () => SmeltDragItem.new("x", "row").on_drop(1)]) {
      try { bad(); } catch (_) { rejected++; }
    }
    const item = (id, kind) => SmeltDragItem.new(id, kind).w(200).h(30)
      .content(div().size_full().child(`row ${id}`))
      .on_drop((from, before, top, cx) => {
        this.calls.push(`${from}${before ? "<" : ">"}${id}@${top}`);
        const rest = this.items.filter((it) => it !== from);
        const at = rest.indexOf(id) + (before ? 0 : 1);
        rest.splice(at, 0, from);
        this.items = rest;
        cx.notify();
      });
    return div().size_full().flex().flex_col()
      .child(SmeltScrollView.new("list").h(200)
        .on_drop_files((paths, cx) => { this.files = paths.join(","); cx.notify(); })
        .children([...this.items.map((id) => item(id, "row")), item("d", "other")]))
      .child(`rejected: ${rejected}; order: ${this.items.join(",")}; calls: ${this.calls.join(" ")}; files: ${this.files}`);
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
    assert!(state(&mut context).starts_with("rejected: 2; order: a,b,c; calls: ;"));

    // 第一项拖到第三项的下半：放在它后面。
    drag(&mut context, 15., 80.);
    assert!(
        state(&mut context).contains("order: b,c,a; calls: a>c@61;"),
        "{}",
        state(&mut context)
    );

    // 最后一项拖到第一项的上半：放在它前面。
    drag(&mut context, 75., 5.);
    assert!(
        state(&mut context).contains("order: a,b,c; calls: a>c@61 a<b@-14;"),
        "{}",
        state(&mut context)
    );

    // 落回自己上、别的种类拖进来、松在外面：都不报。
    drag(&mut context, 45., 50.);
    drag(&mut context, 105., 15.);
    drag(&mut context, 45., 300.);
    assert!(
        state(&mut context).contains("calls: a>c@61 a<b@-14;"),
        "{}",
        state(&mut context)
    );

    // 从访达拖文件进滚动区域。
    let position = point(px(10.), px(40.));
    context.simulate_event(gpui::FileDropEvent::Entered {
        position,
        paths: gpui::ExternalPaths(
            ["/tmp/one", "/tmp/two"]
                .map(Into::into)
                .into_iter()
                .collect(),
        ),
    });
    draw(&mut context);
    context.simulate_event(gpui::FileDropEvent::Submit { position });
    draw(&mut context);
    assert!(
        state(&mut context).ends_with("files: /tmp/one,/tmp/two"),
        "{}",
        state(&mut context)
    );
}

/// 拖着停在滚动区域下沿：按帧自动滚下去，松手就停。
#[gpui::test]
fn dragging_at_the_edge_scrolls(cx: &mut TestAppContext) {
    use gpui::MouseButton;

    let source = r#"
import { View, div } from "gpui-kit";
import { SmeltDragItem, SmeltScrollAnchor, SmeltScrollView } from "smelt-ui";
export default class App extends View {
  init() { this.above = ""; }
  render() {
    const rows = [];
    for (let i = 0; i < 20; i++) {
      rows.push(SmeltScrollAnchor.new(`r${i}`).child(
        SmeltDragItem.new(`r${i}`, "row").h(30).content(div().size_full().child(`row ${i}`))));
    }
    return div().size_full().flex().flex_col()
      .child(SmeltScrollView.new("list").h(100).watch(["r0"])
        .on_offscreen((above, _below, cx) => { this.above = above.join(","); cx.notify(); })
        .children(rows))
      .child(div().child(`above: ${this.above};`));
  }
}
"#;
    let (mut context, view, _app) = mount(cx, source);
    draw(&mut context);
    let (from, edge) = (point(px(10.), px(15.)), point(px(10.), px(98.)));
    context.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
    context.simulate_mouse_move(
        point(px(10.), px(19.)),
        MouseButton::Left,
        Modifiers::default(),
    );
    draw(&mut context);
    context.simulate_mouse_move(edge, MouseButton::Left, Modifiers::default());
    draw(&mut context);
    context
        .executor()
        .advance_clock(std::time::Duration::from_millis(500));
    draw(&mut context);
    draw(&mut context);
    let during = tree(&mut context, &view);
    assert!(during.contains("above: r0;"), "{during}");
    context.simulate_mouse_up(edge, MouseButton::Left, Modifiers::default());
    draw(&mut context);
}

/// 按下第 `y` 像素那一行的按钮，读它报的名字。
fn hit(context: &mut VisualTestContext, view: &Entity<ScriptView>, y: f32) -> String {
    click(context, 100., y);
    let tree = tree(context, view);
    let start = tree.find("hit: ").expect("状态行");
    tree[start..]
        .lines()
        .next()
        .unwrap()
        .trim_end_matches('"')
        .to_string()
}

/// 一组合上：第一帧后面的行不动，按弹簧慢慢上去；减弱动态效果时一下到位。
#[gpui::test]
fn a_fold_closes_smoothly_and_at_once_under_reduced_motion(cx: &mut TestAppContext) {
    let source = r#"
import { View, div } from "gpui-kit";
import { SmeltButton, SmeltFold, SmeltFoldChevron } from "smelt-ui";
export default class App extends View {
  init() { this.collapsed = false; this.hit = ""; }
  render() {
    const button = (name, h) => SmeltButton.new(name).label(name).w(200).h(h)
      .on_click((_, cx) => { this.hit = name; if (name === "toggle") this.collapsed = !this.collapsed; cx.notify(); });
    return div().flex().flex_col().w(200)
      .child(button("toggle", 30))
      .child(SmeltFoldChevron.new("g", this.collapsed).size(14))
      .child(SmeltFold.new("g", this.collapsed).child(button("inside", 120)))
      .child(button("after", 30))
      .child(div().child(`hit: ${this.hit}`));
  }
}
"#;
    let (mut context, view, _app) = mount(cx, source);
    context.update(|_, cx| cx.set_reduce_motion(false));
    draw(&mut context);
    // 开着：toggle 0–30，箭头 30–44，组里 44–164，after 164–194。
    assert_eq!(hit(&mut context, &view, 100.), "hit: inside");
    assert_eq!(hit(&mut context, &view, 10.), "hit: toggle");
    assert_eq!(
        hit(&mut context, &view, 180.),
        "hit: after",
        "合上第一帧后面的行不动"
    );
    context
        .executor()
        .advance_clock(std::time::Duration::from_secs(2));
    draw(&mut context);
    assert_eq!(
        hit(&mut context, &view, 60.),
        "hit: after",
        "停稳后组里不画了"
    );

    // 减弱动态效果：再开再合都一下到位。
    context.update(|_, cx| cx.set_reduce_motion(true));
    assert_eq!(hit(&mut context, &view, 10.), "hit: toggle");
    assert_eq!(hit(&mut context, &view, 100.), "hit: inside");
    assert_eq!(hit(&mut context, &view, 10.), "hit: toggle");
    assert_eq!(hit(&mut context, &view, 60.), "hit: after");
}

/// 重排后各项先画在原处再滑到新位置，点击跟着画的地方走；新落进来的一行从零高撑开。
#[gpui::test]
fn flips_slide_and_arrivals_open(cx: &mut TestAppContext) {
    let source = r#"
import { View, div } from "gpui-kit";
import { SmeltButton, SmeltFlip, SmeltArrival, SmeltWash, SmeltGlowIcon } from "smelt-ui";
export default class App extends View {
  init() { this.items = ["a", "b"]; this.generation = 0; this.arrived = null; this.hit = ""; }
  render() {
    const button = (name) => SmeltButton.new(name).label(name).w(200).h(30)
      .on_click((_, cx) => {
        this.hit = name;
        if (name === "swap") { this.items = [...this.items].reverse(); this.generation += 1; }
        if (name === "add") { this.items = [...this.items, "c"]; this.arrived = "c"; }
        cx.notify();
      });
    const row = (id) => {
      const item = div().relative().child(SmeltWash.new(id, 1)).child(button(id))
        .child(SmeltGlowIcon.new("icons/terminal.svg", id, 1).size(14));
      const placed = this.arrived === id ? SmeltArrival.new(id, 1).child(item) : item;
      return SmeltFlip.new(id, this.generation).child(placed);
    };
    return div().flex().flex_col().w(200)
      .child(button("swap"))
      .child(button("add"))
      .children(this.items.map(row))
      .child(button("end"))
      .child(div().child(`hit: ${this.hit}`));
  }
}
"#;
    let (mut context, view, _app) = mount(cx, source);
    context.update(|_, cx| cx.set_reduce_motion(false));
    draw(&mut context);
    // swap 0–30，add 30–60，a 60–90，b 90–134（行里还有图标 14），end 之后。
    assert_eq!(hit(&mut context, &view, 75.), "hit: a");
    assert_eq!(hit(&mut context, &view, 10.), "hit: swap");
    assert_eq!(
        hit(&mut context, &view, 75.),
        "hit: a",
        "重排那一帧还画在原处"
    );
    context
        .executor()
        .advance_clock(std::time::Duration::from_secs(1));
    draw(&mut context);
    assert_eq!(hit(&mut context, &view, 75.), "hit: b", "滑到新位置");

    // 新的一行从零高撑开：刚加进来时 end 紧跟在上一行后面。
    let end_before = 60. + 44. * 2. + 5.;
    assert_eq!(hit(&mut context, &view, 45.), "hit: add");
    assert_eq!(hit(&mut context, &view, end_before), "hit: end");
    context
        .executor()
        .advance_clock(std::time::Duration::from_secs(2));
    draw(&mut context);
    assert_eq!(hit(&mut context, &view, end_before), "hit: c", "撑开到全高");
}
