use std::sync::{Arc, Mutex};

use gpui::{Modifiers, TestAppContext, VisualTestContext, point, px};
use gpui_shell::{
    ArgumentSchema, COMPONENT_REGISTRY_API_VERSION, ComponentDescriptor, ComponentMaterializer,
    ComponentRegistry, MaterializeRequest, anyhow,
};

use crate::js::ui::tests::{draw, mount, mount_with_registry, tree};

#[test]
fn shell_button_accessibility_descriptors() {
    let mut registry = ComponentRegistry::new(COMPONENT_REGISTRY_API_VERSION, "smelt-ui").unwrap();
    super::register(&mut registry).unwrap();
    let registry = registry.freeze().unwrap();
    let button = registry
        .descriptors()
        .find(|d| d.name() == "SmeltButton")
        .unwrap();
    let loading = button
        .methods()
        .iter()
        .find(|m| m.name() == "loading")
        .unwrap();
    assert_eq!(loading.arguments().len(), 1);
    assert_eq!(loading.arguments()[0].schema(), &ArgumentSchema::Boolean);
    assert!(loading.documentation().is_some());
    for name in ["accessibility_label", "tooltip"] {
        let method = button.methods().iter().find(|m| m.name() == name).unwrap();
        assert_eq!(method.arguments().len(), 1);
        assert_eq!(method.arguments()[0].name(), name);
        assert_eq!(method.arguments()[0].schema(), &ArgumentSchema::String);
        assert!(method.documentation().is_some());
    }
}

// 观察 Shell 交给真实 materializer 的 payload；这不是原生无障碍树断言。
struct RecordingMaterializer(Arc<Mutex<Vec<Vec<(String, String)>>>>);

impl ComponentMaterializer for RecordingMaterializer {
    fn materialize(&self, request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        let operations = request
            .methods()
            .filter_map(|method| {
                let op = method.payload().downcast_ref::<super::ButtonOp>()?;
                match op {
                    super::ButtonOp::AccessibilityLabel(value) => {
                        Some(("accessibility_label".into(), value.clone()))
                    }
                    super::ButtonOp::Tooltip(value) => Some(("tooltip".into(), value.clone())),
                    _ => None,
                }
            })
            .collect();
        self.0.lock().unwrap().push(operations);
        super::ButtonMaterializer.materialize(request)
    }
}

#[gpui::test]
fn shell_button_icon_child_records_accessibility_methods(cx: &mut TestAppContext) {
    let source = r#"
import { View, div } from "gpui-kit";
import { SmeltButton as Button } from "smelt-ui";
export default class App extends View {
  init() { this.hits = 0; }
  render() {
    return div().size_full()
      .child(new Button("close").accessibility_label("Old name").tooltip("Old help")
        .accessibility_label("Close panel").tooltip("Close this panel")
        .child(div().child("×")).absolute().left(0).top(0).w(40).h(28)
        .on_click((_event, cx) => { this.hits++; cx.notify(); }))
      .child(`hits: ${this.hits}`);
  }
}
"#;
    let mut source_registry =
        ComponentRegistry::new(COMPONENT_REGISTRY_API_VERSION, "smelt-ui").unwrap();
    super::register(&mut source_registry).unwrap();
    let source_registry = source_registry.freeze().unwrap();
    let descriptor = source_registry
        .descriptors()
        .find(|d| d.name() == "SmeltButton")
        .unwrap();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let mut registry = ComponentRegistry::new(COMPONENT_REGISTRY_API_VERSION, "smelt-ui").unwrap();
    registry
        .register(
            ComponentDescriptor::new(
                "SmeltButton",
                Arc::new(RecordingMaterializer(captured.clone())),
            )
            .with_constructors(descriptor.constructors().to_vec())
            .with_methods(descriptor.methods().to_vec()),
        )
        .unwrap();
    let (mut context, view, _app) = mount_with_registry(cx, source, registry);
    draw(&mut context);
    let recorded = captured
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("materialized");
    assert_eq!(
        recorded,
        vec![
            ("accessibility_label".into(), "Old name".into()),
            ("tooltip".into(), "Old help".into()),
            ("accessibility_label".into(), "Close panel".into()),
            ("tooltip".into(), "Close this panel".into()),
        ]
    );
    let before = tree(&mut context, &view);
    assert!(
        before.contains(":accessibility_label(registered)"),
        "{before}"
    );
    assert!(before.contains(":tooltip(registered)"), "{before}");
    assert!(before.contains("×"), "{before}");
    assert!(before.contains("hits: 0"), "{before}");
    context.simulate_click(point(px(10.), px(14.)), Modifiers::default());
    draw(&mut context);
    let after = tree(&mut context, &view);
    assert!(after.contains("hits: 1"), "{after}");
}

#[gpui::test]
fn shell_button_properties_and_clicks(cx: &mut TestAppContext) {
    let source = r#"
import { View, div } from "gpui-kit";
import { SmeltButton as Button } from "smelt-ui";
export default class App extends View {
  init() { this.hits = [0, 0, 0, 0, 0, 0]; this.locked = true; }
  render() {
    const hit = i => (_event, cx) => { this.hits[i]++; cx.notify(); };
    return div().size_full()
      .child(new Button("primary").label("Primary").primary().small()
        .absolute().left(0).top(0).on_click(hit(0)))
      .child(Button.new("secondary").label("Secondary").primary().secondary().large()
        .absolute().left(0).top(40).disabled(false).on_click(hit(1)))
      .child(new Button("quiet").label("Quiet").quiet()
        .absolute().left(0).top(80).on_click(hit(2)))
      .child(new Button("danger").label("Danger").danger()
        .absolute().left(0).top(120).on_click(hit(3)))
      .child(new Button("danger-quiet").label("Danger quiet").danger_quiet()
        .absolute().left(0).top(160).on_click(hit(4)))
      .child(new Button("disabled").label("Disabled").disabled(this.locked)
        .absolute().left(0).top(200).on_click(hit(5)))
      .child(new Button("unlock").label("Unlock").absolute().left(0).top(240)
        .on_click((_event, cx) => { this.locked = false; cx.notify(); }))
      .child(`hits: ${this.hits.join("|")}`);
  }
}
"#;
    let (mut context, view, _app) = mount(cx, source);
    draw(&mut context);
    let initial = tree(&mut context, &view);
    assert!(initial.contains("hits: 0|0|0|0|0|0"), "{initial}");
    for method in [
        "label",
        "primary",
        "secondary",
        "quiet",
        "danger",
        "danger_quiet",
        "small",
        "large",
    ] {
        assert!(
            initial.contains(&format!(":{method}(registered)")),
            "{initial}"
        );
    }

    // 小号 24px、 大号 32px。点在默认尺寸或无标签图标按钮以外的区域，
    // 同时检查 label 和 size 到真实组件的传递（不覆盖组件自己的高度）。
    for y in [22., 70., 94., 134., 174., 214.] {
        context.simulate_click(point(px(10.), px(y)), Modifiers::default());
        draw(&mut context);
    }
    let clicked = tree(&mut context, &view);
    assert!(clicked.contains("hits: 1|1|1|1|1|0"), "{clicked}");

    // 更新脚本状态并重新物化：同一个 id 从 disabled(true) 变成 false。
    context.simulate_click(point(px(10.), px(254.)), Modifiers::default());
    draw(&mut context);
    context.simulate_click(point(px(10.), px(214.)), Modifiers::default());
    draw(&mut context);
    let enabled = tree(&mut context, &view);
    assert!(enabled.contains("hits: 1|1|1|1|1|1"), "{enabled}");
}

#[gpui::test]
fn shell_button_loading_blocks_clicks_and_preserves_width(cx: &mut TestAppContext) {
    let source = r#"
import { View, div } from "gpui-kit";
import { SmeltButton as Button } from "smelt-ui";
export default class App extends View {
  init() { this.loading = true; this.locked = false; this.hits = 0; }
  render() {
    return div().size_full()
      .child(Button.new("target").label("Choose directory").small()
        .loading(false).loading(this.loading).disabled(this.locked).absolute().left(30).top(40)
        .on_click((_event, cx) => { this.hits++; cx.notify(); }))
      .child(Button.new("toggle").label("Toggle").absolute().left(30).top(80)
        .on_click((_event, cx) => { this.loading = !this.loading; cx.notify(); }))
      .child(Button.new("lock").label("Lock").absolute().left(30).top(120)
        .on_click((_event, cx) => { this.locked = true; cx.notify(); }))
      .child(`hits: ${this.hits}`);
  }
}
"#;
    let (mut context, view, _app) = mount(cx, source);
    // 加载时底换成 `soft_hover`，平时是次要按钮的底。
    let loading_fill: gpui::Background = gpui::rgb(smelt_ui::ui_theme::soft_hover()).into();
    let rest_fill: gpui::Background = smelt_ui::ui_theme::palette().button_secondary[0].into();
    let width = |context: &mut VisualTestContext, background: gpui::Background| {
        context.update(|window, _| {
            let scale = window.scale_factor();
            window
                .painted_quads()
                .iter()
                .find(|quad| {
                    (quad.bounds.origin.x.0 - 30. * scale).abs() < 1.
                        && (quad.bounds.origin.y.0 - 40. * scale).abs() < 1.
                        && (quad.bounds.size.height.0 - 24. * scale).abs() < 1.
                        && quad.background == background
                })
                .expect("target background")
                .bounds
                .size
                .width
                .0
        })
    };
    draw(&mut context);
    let loading_width = width(&mut context, loading_fill);
    context.simulate_click(point(px(40.), px(52.)), Modifiers::default());
    draw(&mut context);
    assert!(tree(&mut context, &view).contains("hits: 0"));
    context.simulate_click(point(px(40.), px(94.)), Modifiers::default());
    draw(&mut context);
    assert!((loading_width - width(&mut context, rest_fill)).abs() < 1.);
    context.simulate_click(point(px(40.), px(52.)), Modifiers::default());
    draw(&mut context);
    assert!(tree(&mut context, &view).contains("hits: 1"));
    context.simulate_click(point(px(40.), px(134.)), Modifiers::default());
    draw(&mut context);
    context.simulate_click(point(px(40.), px(52.)), Modifiers::default());
    draw(&mut context);
    assert!(tree(&mut context, &view).contains("hits: 1"));
}

#[gpui::test]
fn shell_button_paints_roles_and_sizes(cx: &mut TestAppContext) {
    let source = r#"
import { View, div } from "gpui-kit";
import { SmeltButton as Button } from "smelt-ui";
export default class App extends View {
  render() {
    return div().size_full()
      .child(new Button("p").label("Primary").primary().small().absolute().left(30).top(40).w(180))
      .child(new Button("s").label("Secondary").primary().secondary().small().large().absolute().left(30).top(80).w(180))
      .child(new Button("q").label("Quiet").quiet().absolute().left(30).top(120).w(180))
      .child(new Button("d").label("Danger").danger().absolute().left(30).top(160).w(180))
      .child(new Button("dq").label("Danger quiet").danger_quiet().absolute().left(30).top(200).w(180))
      .child(new Button("default").label("Default").absolute().left(30).top(240).w(180))
      .child(new Button("short").label("a").small().absolute().left(30).top(280))
      .child(new Button("long").label("A much longer label").small().absolute().left(30).top(320))
      .child(new Button("relabel").label("A much longer label").label("a").small().absolute().left(30).top(360));
  }
}
"#;
    let (mut context, _view, _app) = mount(cx, source);
    draw(&mut context);
    context.update(|window, _| {
        let scale = window.scale_factor();
        let quads = window.painted_quads();
        let palette = smelt_ui::ui_theme::palette();
        let primary: gpui::Background = palette.button_primary[0].into();
        let secondary: gpui::Background = palette.button_secondary[0].into();
        for (top, height, background) in [
            (40., 24., Some(primary)),
            (80., 36., Some(secondary)),
            (120., 28., None),
            (160., 28., Some(secondary)),
            (200., 28., None),
            (240., 28., Some(secondary)),
        ] {
            let matching = quads.iter().filter(|quad| {
                (quad.bounds.origin.x.0 - 30. * scale).abs() < 1.
                    && (quad.bounds.origin.y.0 - top * scale).abs() < 1.
                    && (quad.bounds.size.width.0 - 180. * scale).abs() < 1.
                    && (quad.bounds.size.height.0 - height * scale).abs() < 1.
            });
            if let Some(background) = background {
                assert!(
                    matching
                        .into_iter()
                        .any(|quad| quad.background == background),
                    "top={top}, quads={quads:?}"
                );
            } else {
                assert!(
                    !matching
                        .into_iter()
                        .any(|quad| quad.background.as_solid().is_some_and(|color| color.a > 0.)),
                    "quiet button at top={top} has a background: {quads:?}"
                );
            }
        }
        let width_at = |top: f32| {
            quads
                .iter()
                .find(|quad| {
                    (quad.bounds.origin.x.0 - 30. * scale).abs() < 1.
                        && (quad.bounds.origin.y.0 - top * scale).abs() < 1.
                        && (quad.bounds.size.height.0 - 24. * scale).abs() < 1.
                        && quad.background == secondary
                })
                .expect("labelled button must paint a background")
                .bounds
                .size
                .width
                .0
        };
        let short = width_at(280.);
        let long = width_at(320.);
        let relabel = width_at(360.);
        assert!(long > short, "label must affect button width");
        assert!((short - relabel).abs() < 1., "last label must win");
    });

    // 两个透明角色悬停时都用列表行的悬停底。
    let row_hover = smelt_ui::ui_theme::palette().row_hover;
    for (top, color) in [(120., row_hover), (200., row_hover)] {
        context.simulate_mouse_move(point(px(40.), px(top + 14.)), None, Modifiers::default());
        draw(&mut context);
        context.update(|window, _| {
            let scale = window.scale_factor();
            let background: gpui::Background = color.into();
            assert!(
                window.painted_quads().iter().any(|quad| {
                    (quad.bounds.origin.x.0 - 30. * scale).abs() < 1.
                        && (quad.bounds.origin.y.0 - top * scale).abs() < 1.
                        && (quad.bounds.size.width.0 - 180. * scale).abs() < 1.
                        && (quad.bounds.size.height.0 - 28. * scale).abs() < 1.
                        && quad.background == background
                }),
                "hover background at top={top}"
            );
        });
    }
}

#[gpui::test]
fn shell_button_rejects_invalid_arguments(cx: &mut TestAppContext) {
    let source = r#"
import { View, div } from "gpui-kit";
import { SmeltButton as Button } from "smelt-ui";
export default class App extends View {
  render() {
    let rejected = 0;
    for (const bad of [
      () => new Button(""),
      () => new Button(123),
      () => new Button("label").label(123),
      () => new Button("a11y").accessibility_label(123),
      () => new Button("a11y").accessibility_label(),
      () => new Button("a11y").accessibility_label("Close", "extra"),
      () => new Button("tooltip").tooltip(false),
      () => new Button("tooltip").tooltip(),
      () => new Button("tooltip").tooltip("Help", "extra"),
      () => new Button("disabled").disabled("yes"),
      () => new Button("loading").loading("yes"),
      () => new Button("loading").loading(),
      () => new Button("loading").loading(true, false),
      () => new Button("click").on_click("no"),
    ]) {
      try { bad(); } catch (_) { rejected++; }
    }
    return div().child(`invalid arguments rejected: ${rejected}`);
  }
}
"#;
    let (mut context, view, _app) = mount(cx, source);
    draw(&mut context);
    let snapshot = tree(&mut context, &view);
    assert!(
        snapshot.contains("invalid arguments rejected: 14"),
        "{snapshot}"
    );
}

/// 图标按钮：只给图标是方的（小号 20 见方），关闭控件、浮起的样子也传到真实组件。
#[gpui::test]
fn shell_button_icon_dismiss_and_floating(cx: &mut TestAppContext) {
    let source = r#"
import { View, div } from "gpui-kit";
import { SmeltButton as Button } from "smelt-ui";
export default class App extends View {
  init() { this.hits = [0, 0]; }
  render() {
    const hit = i => (_event, cx) => { this.hits[i]++; cx.notify(); };
    return div().size_full()
      .child(new Button("close").icon("icons/close.svg").accessibility_label("Close").dismiss().small()
        .absolute().left(0).top(0).on_click(hit(0)))
      .child(new Button("pill").icon("icons/arrow-down.svg").label("2 need you").floating().small().dropdown_caret()
        .absolute().left(0).top(40).on_click(hit(1)))
      .child(`hits: ${this.hits.join("|")}`);
  }
}
"#;
    let (mut context, view, _app) = mount(cx, source);
    draw(&mut context);
    let initial = tree(&mut context, &view);
    for method in ["icon", "dismiss", "floating", "dropdown_caret"] {
        assert!(
            initial.contains(&format!(":{method}(registered)")),
            "{initial}"
        );
    }
    // 小号图标按钮 20 见方：10 在里面，24 已经出去了。
    for x in [10., 24.] {
        context.simulate_click(point(px(x), px(10.)), Modifiers::default());
        draw(&mut context);
    }
    context.simulate_click(point(px(10.), px(50.)), Modifiers::default());
    draw(&mut context);
    let clicked = tree(&mut context, &view);
    assert!(clicked.contains("hits: 1|1"), "{clicked}");
}
