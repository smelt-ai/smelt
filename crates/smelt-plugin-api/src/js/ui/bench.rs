//! 探针：功能改用 JS（gpui-shell）写之后，性能花在哪。同一个对话界面 JS 和 Rust 各写一份，
//! 量流式输出、滚动、打字、动画、数据过桥、启动和内存。不是回归测试，默认跳过。跑法：
//!
//! ```text
//! cargo test -j 2 -p smelt-plugin-api --features js --release --lib js::ui::bench -- --ignored --nocapture --test-threads 1
//! ```
//!
//! 每步「做一次动作 + 画一帧」量墙钟时间。测试平台不经 GPU 绘制，两边同一套量法，只看
//! 相对大小和数量级。

use std::{cell::RefCell, ops::Deref, rc::Rc, sync::Arc, time::Duration, time::Instant};

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ListAlignment, ListState, Modifiers, ParentElement as _, Refineable as _, Render, ScrollDelta,
    ScrollEvent, StatefulInteractiveElement as _, Styled as _, TestAppContext, VisualTestContext,
    Window, div, point, px, rgb, size,
};
use gpui_shell::{
    ArgumentDescriptor, ArgumentSchema, ComponentArgument, ComponentDescriptor,
    ComponentMaterializer, ComponentPayload, ComponentRegistry, ConstructorDescriptor,
    FrozenComponentRegistry, HostModule, HostValue, MaterializeRequest, ScriptView, ShellRuntime,
    policy::{self, Policy},
};
use serde_json::{Value, json};
use smelt_ui::markdown_mermaid::markdown_view;

use super::tests::TempApp;

const W: f32 = 800.;
const H: f32 = 900.;
const STEPS: usize = 60;

// ---------------------------------------------------------------- 共用

/// 一条消息的 markdown：标题、列表、代码块和一段正文，像智能体的回复。
fn md(i: usize) -> String {
    format!(
        "**第 {i} 步** 改了 `src/file_{i}.rs`\n\n- 读了文件\n- 改了函数 `run_{i}`\n- 跑了测试\n\n```rust\nfn run_{i}(x: u32) -> u32 {{\n    x * {i} + 1\n}}\n```\n\n{}",
        plain(i)
    )
}

fn plain(i: usize) -> String {
    let words: Vec<_> = WORDS.split(' ').collect();
    let mut s = String::new();
    let mut k = 0;
    while s.len() < 360 {
        s.push_str(words[(i + k) % words.len()]);
        s.push(' ');
        k += 1;
    }
    s
}

const WORDS: &str =
    "the agent reads the file and then edits the function so that the tests pass again";

/// JS 里和上面同样的数据、同样的气泡。`MARKDOWN` 为真时正文交给 Rust 的 markdown 组件。
const PRELUDE: &str = r##"
import { View, div, list } from "gpui-kit";
import { BenchMarkdown } from "smelt-ui";
const WORDS = "the agent reads the file and then edits the function so that the tests pass again".split(" ");
export function plain(i) {
  let s = "";
  for (let k = 0; s.length < 360; k++) s += WORDS[(i + k) % WORDS.length] + " ";
  return s;
}
export function md(i) {
  return `**第 ${i} 步** 改了 \`src/file_${i}.rs\`\n\n- 读了文件\n- 改了函数 \`run_${i}\`\n- 跑了测试\n\n\`\`\`rust\nfn run_${i}(x: u32) -> u32 {\n    x * ${i} + 1\n}\n\`\`\`\n\n${plain(i)}`;
}
export function messages(count, markdown) {
  const out = [];
  for (let i = 0; i < count; i++) out.push({ user: i % 4 === 0, text: markdown ? md(i) : plain(i) });
  return out;
}
export function bubble(i, m, markdown) {
  let chips = div().flex().gap(4);
  for (let c = 0; c < 3; c++) {
    chips = chips.child(div().px(6).py(1).rounded(4).border_1().border_color("#dddddd")
      .text_xs().child(`file_${i}_${c}.rs +${c * 3} -${c}`));
  }
  return div().id(`m${i}`).flex().flex_col().gap(4).mx(12).my(4).px(12).py(8).rounded(8)
    .bg(m.user ? "#eef2ff" : "#ffffff").border_1().border_color("#e5e5e5")
    .child(div().flex().items_center().gap(6).text_xs().text_color("#888888")
      .child(m.user ? "You" : "Agent").child(`#${i}`))
    .child(markdown
      ? BenchMarkdown.new(`md${i}`, m.text)
      : div().text_sm().text_color("#111111").child(m.text))
    .child(chips);
}
export function tick(handler) {
  return div().id("tick").absolute().left(0).top(0).w(40).h(20).bg("#ff0000").on_click(handler);
}
"##;

/// 对话：`__MODE__` 是 full（整段描述）、list（虚拟列表）、split（每条一个子视图）。
const CONVERSATION: &str = r##"
class Bubble extends View {
  init(props) { this.i = props.i; this.m = props.m; this.markdown = props.markdown; }
  render() { return bubble(this.i, this.m, this.markdown); }
}
export default class App extends View {
  init(_props, cx) {
    this.mode = "__MODE__";
    this.markdown = __MARKDOWN__;
    this.messages = messages(__COUNT__, this.markdown);
    if (this.mode === "split") {
      this.views = this.messages.map((m, i) => cx.new(Bubble, { i, m, markdown: this.markdown }));
    }
  }
  body() {
    if (this.mode === "list") {
      return list("conv", this.messages.length, (i) => `m${i}`, (i) => bubble(i, this.messages[i], this.markdown))
        .with_follow_mode("tail").size_full();
    }
    let col = div().id("scroll").size_full().flex().flex_col().overflow_y_scroll();
    for (let i = 0; i < this.messages.length; i++) {
      col = col.child(this.mode === "split" ? this.views[i] : bubble(i, this.messages[i], this.markdown));
    }
    return col;
  }
  render() {
    return div().size_full().relative().child(this.body()).child(tick((_e, cx) => {
      const last = this.messages.length - 1;
      this.messages[last].text += "token ";
      if (this.mode === "split") cx.notify(this.views[last]); else cx.notify();
    }));
  }
}
"##;

/// 对话旁边有输入框，每敲一个字整个视图重新描述一次（列表只描述看得见的）。
const TYPING: &str = r##"
import { SmeltInputState, SmeltTextInput } from "smelt-ui";
export default class App extends View {
  init() {
    this.messages = messages(2000, false);
    this.query = "";
    this.input = SmeltInputState.new("Message");
    this.input.on("change", (_e, cx) => { this.query = this.input.value(); cx.notify(); });
  }
  render() {
    return div().size_full().flex().flex_col()
      .child(SmeltTextInput.new(this.input).aria_label("Message").w(400).h(30))
      .child(div().text_xs().child(`${this.query.length} chars`))
      .child(list("conv", this.messages.length, (i) => `m${i}`, (i) => bubble(i, this.messages[i], false))
        .with_follow_mode("tail").flex_1());
  }
}
"##;

/// 50 行高度过渡：点一下全部变高，动画由原生跑。
const ANIMATION: &str = r##"
export default class App extends View {
  init() { this.open = false; }
  render() {
    let col = div().flex().flex_col();
    for (let i = 0; i < 50; i++) {
      col = col.child(div().id(`r${i}`).w(300).h(this.open ? 16 : 8).transition("height", 300)
        .bg("#eeeeee").child(`row ${i}`));
    }
    return div().size_full().relative().child(col).child(tick((_e, cx) => { this.open = !this.open; cx.notify(); }));
  }
}
"##;

/// 数据过桥：每步从宿主拿一份 2000 条的会话快照（约 1MB），或把它交回宿主，或在 JS 里解析
/// 同样大小的 JSON 字符串。
const BRIDGE: &str = r##"
import { snapshot, sink } from "bench-host";
export default class App extends View {
  init() { this.n = 0; this.mode = "__MODE__"; }
  json() {
    if (!this.big) this.big = JSON.stringify(snapshot());
    return this.big;
  }
  render() {
    return div().size_full().relative().child(`${this.n}`).child(tick((_e, cx) => {
      if (this.mode === "get") this.data = snapshot();
      else if (this.mode === "put") { if (!this.data) this.data = snapshot(); sink(this.data); }
      else this.data = JSON.parse(this.json());
      this.n = this.data.length;
      cx.notify();
    }));
  }
}
"##;

fn conversation(mode: &str, count: usize, markdown: bool) -> String {
    format!(
        "{PRELUDE}{}",
        CONVERSATION
            .replace("__MODE__", mode)
            .replace("__COUNT__", &count.to_string())
            .replace("__MARKDOWN__", &markdown.to_string())
    )
}

fn snapshot_json() -> Value {
    Value::Array(
        (0..2000)
            .map(|i| {
                json!({
                    "id": format!("m{i}"), "role": "agent", "text": md(i),
                    "tools": [{ "name": "edit", "path": format!("/repo/src/f{i}.rs"), "added": i, "removed": 1 }],
                })
            })
            .collect(),
    )
}

// ---------------------------------------------------------------- JS 挂载

/// `BenchMarkdown(id, text)`：Rust 的 markdown 渲染，给 JS 当组件用。
#[derive(Clone)]
struct MdPayload {
    id: String,
    text: String,
}

struct MdMaterializer;

impl ComponentMaterializer for MdMaterializer {
    fn materialize(
        &self,
        mut request: MaterializeRequest<'_>,
    ) -> gpui_shell::anyhow::Result<AnyElement> {
        let MdPayload { id, text } = request
            .payload()
            .downcast_ref::<MdPayload>()
            .cloned()
            .ok_or_else(|| gpui_shell::anyhow::anyhow!("BenchMarkdown payload"))?;
        let mut wrapper = div().text_sm().child(markdown_view(id, text));
        wrapper.style().refine(&request.take_style());
        Ok(wrapper.into_any_element())
    }
}

fn registry() -> FrozenComponentRegistry {
    let mut registry =
        ComponentRegistry::new(super::COMPONENT_REGISTRY_API_VERSION, super::MODULE).unwrap();
    super::button::register(&mut registry).unwrap();
    super::menu::register(&mut registry).unwrap();
    super::confirm::register(&mut registry).unwrap();
    super::choice::register(&mut registry).unwrap();
    super::field::register(&mut registry).unwrap();
    super::alert::register(&mut registry).unwrap();
    super::badge::register(&mut registry).unwrap();
    super::view::register(&mut registry).unwrap();
    super::window_bar::register(&mut registry).unwrap();
    super::sortable::register(&mut registry).unwrap();
    registry
        .register(
            ComponentDescriptor::new("BenchMarkdown", Arc::new(MdMaterializer))
                .with_constructors(vec![ConstructorDescriptor::new(
                    "BenchMarkdown",
                    vec![
                        ArgumentDescriptor::new("id", ArgumentSchema::String),
                        ArgumentDescriptor::new("text", ArgumentSchema::String),
                    ],
                    |args| match args {
                        [
                            ComponentArgument::String(id),
                            ComponentArgument::String(text),
                        ] => Ok(ComponentPayload::new(MdPayload {
                            id: id.clone(),
                            text: text.clone(),
                        })),
                        _ => Err("BenchMarkdown(id, text)".into()),
                    },
                )])
                .with_documentation("Markdown rendered by Rust, for the probe."),
        )
        .unwrap();
    registry.freeze().unwrap()
}

fn bench_policy() -> Policy {
    let data = Rc::new(super::super::host_value(&snapshot_json()));
    Policy::new()
        .with_host_module(
            HostModule::new("bench-host")
                .function("snapshot", move |_| Ok((*data).clone()))
                .function("sink", |arguments| {
                    let value = super::super::json_value(arguments.value(0)?);
                    std::hint::black_box(&value);
                    Ok(HostValue::Null)
                }),
        )
        .unwrap()
}

struct Mounted {
    runtime: Rc<ShellRuntime>,
    context: VisualTestContext,
    _view: Entity<ScriptView>,
    _app: TempApp,
}

fn init(cx: &mut TestAppContext, reduce_motion: bool) {
    cx.update(|cx| {
        gpui_shell::init(cx);
        smelt_ui::init(cx);
        cx.set_reduce_motion(reduce_motion);
    });
}

fn mount(cx: &mut TestAppContext, source: &str) -> Mounted {
    let runtime = ShellRuntime::new_isolated_with_components(registry()).unwrap();
    mount_in(cx, &runtime, source)
}

fn mount_in(cx: &mut TestAppContext, runtime: &Rc<ShellRuntime>, source: &str) -> Mounted {
    policy::set_default(bench_policy());
    let app = TempApp::new(source);
    let loaded = runtime.load_application(&app.0, "main.js").unwrap();
    let mounted = Rc::new(RefCell::new(None));
    let slot = mounted.clone();
    let rt = runtime.clone();
    let window = cx.open_window(size(px(W), px(H)), move |window, cx| {
        let view = rt.mount_application(&loaded, window, cx).unwrap();
        *slot.borrow_mut() = Some(view.clone());
        gpui_base::Root::new(view, window, cx)
    });
    let context = VisualTestContext::from_window(*window.deref(), cx);
    let view = mounted.borrow().clone().unwrap();
    Mounted {
        runtime: runtime.clone(),
        context,
        _view: view,
        _app: app,
    }
}

fn draw(context: &mut VisualTestContext) {
    context.run_until_parked();
    context.update(|window, cx| window.draw(cx).clear(cx));
}

fn wheel(context: &mut VisualTestContext) {
    context.simulate_event(ScrollEvent {
        position: point(px(W / 2.), px(H / 2.)),
        delta: ScrollDelta::Pixels(point(px(0.), px(-60.))),
        ..Default::default()
    });
}

fn click_tick(context: &mut VisualTestContext) {
    context.simulate_click(point(px(10.), px(10.)), Modifiers::default());
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.
}

struct Stats(Vec<Duration>);

impl Stats {
    fn show(mut self) -> String {
        self.0.sort();
        let n = self.0.len();
        format!(
            "中位 {:>7.2}  p95 {:>7.2}  p99 {:>7.2}  最慢 {:>7.2} ms",
            ms(self.0[n / 2]),
            ms(self.0[n * 95 / 100]),
            ms(self.0[n * 99 / 100]),
            ms(self.0[n - 1])
        )
    }
}

/// 每一步：做一次动作再画一帧，量整段墙钟时间。前 5 步热身不算。
fn run(
    context: &mut VisualTestContext,
    steps: usize,
    mut step: impl FnMut(&mut VisualTestContext),
) -> Stats {
    for _ in 0..5 {
        step(context);
        draw(context);
    }
    let mut times = Vec::new();
    for _ in 0..steps {
        let started = Instant::now();
        step(context);
        draw(context);
        times.push(started.elapsed());
    }
    Stats(times)
}

fn js_case(m: &mut Mounted, label: &str, steps: usize, step: impl FnMut(&mut VisualTestContext)) {
    draw(&mut m.context);
    draw(&mut m.context);
    let before = m.runtime.read_metrics();
    let stats = run(&mut m.context, steps, step);
    let after = m.runtime.read_metrics();
    let renders = after.script_renders() - before.script_renders();
    let script = after.script_render_time() - before.script_render_time();
    let materialize = after.materialize_time() - before.materialize_time();
    println!(
        "JS   {label:<30} {} | 进 JS 渲染 {renders} 次，每次 {:.2} ms | 每步物化 {:.2} ms",
        stats.show(),
        ms(script) / renders.max(1) as f64,
        ms(materialize) / (steps + 5) as f64,
    );
}

fn rss_mb() -> f64 {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<f64>()
        .unwrap()
        / 1024.
}

// ---------------------------------------------------------------- Rust 对照

struct Message {
    user: bool,
    text: String,
}

struct RustConv {
    messages: Rc<RefCell<Vec<Message>>>,
    list: Option<ListState>,
    markdown: bool,
}

fn bubble(i: usize, m: &Message, markdown: bool) -> AnyElement {
    let mut chips = div().flex().gap(px(4.));
    for c in 0..3 {
        chips = chips.child(
            div()
                .px(px(6.))
                .py(px(1.))
                .rounded(px(4.))
                .border_1()
                .border_color(rgb(0xdddddd))
                .text_xs()
                .child(format!("file_{i}_{c}.rs +{} -{c}", c * 3)),
        );
    }
    let body = if markdown {
        div()
            .text_sm()
            .child(markdown_view(format!("md{i}"), m.text.clone()))
    } else {
        div()
            .text_sm()
            .text_color(rgb(0x111111))
            .child(m.text.clone())
    };
    div()
        .id(("m", i))
        .flex()
        .flex_col()
        .gap(px(4.))
        .mx(px(12.))
        .my(px(4.))
        .px(px(12.))
        .py(px(8.))
        .rounded(px(8.))
        .bg(rgb(if m.user { 0xeef2ff } else { 0xffffff }))
        .border_1()
        .border_color(rgb(0xe5e5e5))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .text_xs()
                .text_color(rgb(0x888888))
                .child(if m.user { "You" } else { "Agent" })
                .child(format!("#{i}")),
        )
        .child(body)
        .child(chips)
        .into_any_element()
}

impl Render for RustConv {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let markdown = self.markdown;
        if let Some(state) = &self.list {
            let messages = self.messages.clone();
            return gpui::list(state.clone(), move |ix, _, _| {
                bubble(ix, &messages.borrow()[ix], markdown)
            })
            .size_full()
            .into_any_element();
        }
        div()
            .id("scroll")
            .size_full()
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .children(
                self.messages
                    .borrow()
                    .iter()
                    .enumerate()
                    .map(|(i, m)| bubble(i, m, markdown)),
            )
            .into_any_element()
    }
}

fn rust_case(
    cx: &mut TestAppContext,
    label: &str,
    use_list: bool,
    markdown: bool,
    count: usize,
    scroll: bool,
) {
    init(cx, true);
    let messages = Rc::new(RefCell::new(
        (0..count)
            .map(|i| Message {
                user: i % 4 == 0,
                text: if markdown { md(i) } else { plain(i) },
            })
            .collect::<Vec<_>>(),
    ));
    let list = use_list.then(|| ListState::new(count, ListAlignment::Bottom, px(500.)));
    let view_slot: Rc<RefCell<Option<Entity<RustConv>>>> = Rc::default();
    let slot = view_slot.clone();
    let (m2, l2) = (messages.clone(), list.clone());
    let window = cx.open_window(size(px(W), px(H)), move |window, cx| {
        let view = cx.new(|_| RustConv {
            messages: m2,
            list: l2,
            markdown,
        });
        *slot.borrow_mut() = Some(view.clone());
        gpui_base::Root::new(view, window, cx)
    });
    let mut context = VisualTestContext::from_window(*window.deref(), cx);
    let view = view_slot.borrow().clone().unwrap();
    draw(&mut context);
    draw(&mut context);
    let stats = run(&mut context, STEPS, |context| {
        if scroll {
            wheel(context);
            return;
        }
        let last = count - 1;
        messages.borrow_mut()[last].text.push_str("token ");
        if let Some(state) = &list {
            state.splice(last..count, 1);
        }
        context.update(|_, cx: &mut App| view.update(cx, |_, cx| cx.notify()));
    });
    println!("Rust {label:<30} {}", stats.show());
}

// ---------------------------------------------------------------- 渲染

#[gpui::test]
#[ignore = "性能探针，手动跑"]
fn p01_stream_full_50(cx: &mut TestAppContext) {
    init(cx, true);
    let mut m = mount(cx, &conversation("full", 50, false));
    js_case(&mut m, "流式 整段描述 50 条", STEPS, click_tick);
}

#[gpui::test]
#[ignore = "性能探针，手动跑"]
fn p02_stream_full_200(cx: &mut TestAppContext) {
    init(cx, true);
    let mut m = mount(cx, &conversation("full", 200, false));
    js_case(&mut m, "流式 整段描述 200 条", STEPS, click_tick);
}

#[gpui::test]
#[ignore = "性能探针，手动跑"]
fn p03_rust_stream_full_200(cx: &mut TestAppContext) {
    rust_case(cx, "流式 整段描述 200 条", false, false, 200, false);
}

#[gpui::test]
#[ignore = "性能探针，手动跑"]
fn p04_stream_split_200(cx: &mut TestAppContext) {
    init(cx, true);
    let mut m = mount(cx, &conversation("split", 200, false));
    js_case(&mut m, "流式 每条子视图 200 条", STEPS, click_tick);
}

#[gpui::test]
#[ignore = "性能探针，手动跑"]
fn p05_stream_list_2000(cx: &mut TestAppContext) {
    init(cx, true);
    let mut m = mount(cx, &conversation("list", 2000, false));
    js_case(&mut m, "流式 虚拟列表 2000 条", STEPS, click_tick);
}

#[gpui::test]
#[ignore = "性能探针，手动跑"]
fn p06_rust_stream_list_2000(cx: &mut TestAppContext) {
    rust_case(cx, "流式 虚拟列表 2000 条", true, false, 2000, false);
}

#[gpui::test]
#[ignore = "性能探针，手动跑"]
fn p07_stream_list_markdown_2000(cx: &mut TestAppContext) {
    init(cx, true);
    let mut m = mount(cx, &conversation("list", 2000, true));
    js_case(&mut m, "流式 虚拟列表+markdown 2000", STEPS, click_tick);
}

#[gpui::test]
#[ignore = "性能探针，手动跑"]
fn p08_rust_stream_list_markdown_2000(cx: &mut TestAppContext) {
    rust_case(cx, "流式 虚拟列表+markdown 2000", true, true, 2000, false);
}

#[gpui::test]
#[ignore = "性能探针，手动跑"]
fn p09_scroll_list_2000(cx: &mut TestAppContext) {
    init(cx, true);
    let mut m = mount(cx, &conversation("list", 2000, false));
    js_case(&mut m, "滚动 虚拟列表 2000 条", STEPS, wheel);
}

#[gpui::test]
#[ignore = "性能探针，手动跑"]
fn p10_rust_scroll_list_2000(cx: &mut TestAppContext) {
    rust_case(cx, "滚动 虚拟列表 2000 条", true, false, 2000, true);
}

#[gpui::test]
#[ignore = "性能探针，手动跑"]
fn p11_scroll_list_markdown_2000(cx: &mut TestAppContext) {
    init(cx, true);
    let mut m = mount(cx, &conversation("list", 2000, true));
    js_case(&mut m, "滚动 虚拟列表+markdown 2000", STEPS, wheel);
}

#[gpui::test]
#[ignore = "性能探针，手动跑"]
fn p12_rust_scroll_list_markdown_2000(cx: &mut TestAppContext) {
    rust_case(cx, "滚动 虚拟列表+markdown 2000", true, true, 2000, true);
}

#[gpui::test]
#[ignore = "性能探针，手动跑"]
fn p13_long_stream_list_2000(cx: &mut TestAppContext) {
    init(cx, true);
    let mut m = mount(cx, &conversation("list", 2000, false));
    js_case(&mut m, "长跑 流式 2000 步（看 GC）", 2000, click_tick);
}

// ---------------------------------------------------------------- 交互

#[gpui::test]
#[ignore = "性能探针，手动跑"]
fn p14_typing_beside_2000(cx: &mut TestAppContext) {
    init(cx, true);
    let mut m = mount(cx, &format!("{PRELUDE}{TYPING}"));
    draw(&mut m.context);
    m.context
        .simulate_click(point(px(100.), px(15.)), Modifiers::default());
    draw(&mut m.context);
    js_case(&mut m, "打字 旁边列表 2000 条", STEPS, |context| {
        context.simulate_input("a")
    });
}

#[gpui::test]
#[ignore = "性能探针，手动跑"]
fn p15_animation_50_rows(cx: &mut TestAppContext) {
    init(cx, false);
    let mut m = mount(cx, &format!("{PRELUDE}{ANIMATION}"));
    draw(&mut m.context);
    let before = m.runtime.read_metrics();
    let mut frames = Vec::new();
    for _ in 0..5 {
        click_tick(&mut m.context);
        let started = Instant::now();
        while started.elapsed() < Duration::from_millis(350) {
            let frame = Instant::now();
            draw(&mut m.context);
            frames.push(frame.elapsed());
        }
    }
    let after = m.runtime.read_metrics();
    let count = frames.len();
    println!(
        "JS   {:<30} {} | {count} 帧里进 JS 渲染 {} 次",
        "动画 50 行高度过渡",
        Stats(frames).show(),
        after.script_renders() - before.script_renders()
    );
}

// ---------------------------------------------------------------- 数据过桥

fn bridge(cx: &mut TestAppContext, mode: &str, label: &str) {
    init(cx, true);
    let mut m = mount(
        cx,
        &format!("{PRELUDE}{}", BRIDGE.replace("__MODE__", mode)),
    );
    js_case(&mut m, label, STEPS, click_tick);
}

#[gpui::test]
#[ignore = "性能探针，手动跑"]
fn p16_host_to_js(cx: &mut TestAppContext) {
    bridge(cx, "get", "宿主→JS 2000 条快照 ~1MB");
}

#[gpui::test]
#[ignore = "性能探针，手动跑"]
fn p17_js_to_host(cx: &mut TestAppContext) {
    bridge(cx, "put", "JS→宿主 2000 条快照 ~1MB");
}

#[gpui::test]
#[ignore = "性能探针，手动跑"]
fn p18_json_parse(cx: &mut TestAppContext) {
    bridge(cx, "parse", "JS 内 JSON.parse ~1MB");
}

// ---------------------------------------------------------------- 启动和内存

/// 约 `lines` 行的插件：一堆处理数据的函数，加一个视图。
fn big_plugin(seed: usize, lines: usize) -> String {
    let mut s = String::from(PRELUDE);
    let functions = lines / 10;
    for f in 0..functions {
        s.push_str(&format!(
            "function f{seed}_{f}(n) {{\n  const rows = [];\n  for (let i = 0; i < n; i++) {{\n    rows.push({{ id: `r${{i}}`, value: i * {f}, tag: \"t{f}\" }});\n  }}\n  const kept = rows.filter((row) => row.value % 3 === 0);\n  const names = kept.map((row) => `${{row.id}}:${{row.tag}}`);\n  return names.join(\",\").length + {f};\n}}\n\n"
        ));
    }
    s.push_str(&format!(
        "const ALL = [{}];\nexport default class App extends View {{\n  init() {{ this.messages = messages(2000, true); this.sum = ALL.slice(0, 50).reduce((a, f) => a + f(3), 0); }}\n  render() {{ return div().size_full().child(`${{this.sum}}`).child(list(\"conv\", this.messages.length, (i) => `m${{i}}`, (i) => bubble(i, this.messages[i], true)).with_follow_mode(\"tail\").flex_1()); }}\n}}\n",
        (0..functions)
            .map(|f| format!("f{seed}_{f}"))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    s
}

#[gpui::test]
#[ignore = "性能探针，手动跑"]
fn p19_startup_and_memory(cx: &mut TestAppContext) {
    init(cx, true);
    let base = rss_mb();
    let runtime = ShellRuntime::new_isolated_with_components(registry()).unwrap();
    // 一个插件：2 万行代码，持有 2000 条 markdown 消息。
    let source = big_plugin(0, 20_000);
    let started = Instant::now();
    let mut first = mount_in(cx, &runtime, &source);
    let loaded = started.elapsed();
    draw(&mut first.context);
    let shown = started.elapsed();
    let one = rss_mb();
    println!(
        "JS   {:<30} 加载+挂载 {:.1} ms，到第一帧 {:.1} ms | 内存 +{:.0} MB",
        "启动 1 个 2 万行插件",
        ms(loaded),
        ms(shown),
        one - base
    );
    // 再来 9 个，同一个运行时（宿主就是一个进程一个运行时）。
    let started = Instant::now();
    let mut rest = Vec::new();
    for seed in 1..10 {
        let mut m = mount_in(cx, &runtime, &big_plugin(seed, 20_000));
        draw(&mut m.context);
        rest.push(m);
    }
    println!(
        "JS   {:<30} 共 {:.1} ms | 10 个插件内存共 +{:.0} MB",
        "再启动 9 个同样的插件",
        ms(started.elapsed()),
        rss_mb() - base
    );
}

#[gpui::test]
#[ignore = "性能探针，手动跑"]
fn p20_rust_memory_2000_markdown(cx: &mut TestAppContext) {
    let base = rss_mb();
    rust_case(
        cx,
        "流式 虚拟列表+markdown 2000(内存)",
        true,
        true,
        2000,
        false,
    );
    println!(
        "Rust {:<30} 内存 +{:.0} MB",
        "一个对话 2000 条 markdown",
        rss_mb() - base
    );
}

/// 启动拆开看：只加载（解析、编译、跑模块顶层）和挂载（建实例、`init`、第一次描述）各花多少，
/// 随代码行数怎么涨；`init` 里不建消息，排除数据的影响。
#[gpui::test]
#[ignore = "性能探针，手动跑"]
fn p21_startup_scaling(cx: &mut TestAppContext) {
    init(cx, true);
    for lines in [1_000, 5_000, 20_000] {
        let runtime = ShellRuntime::new_isolated_with_components(registry()).unwrap();
        let source = big_plugin(lines, lines).replace(
            "this.messages = messages(2000, true);",
            "this.messages = [];",
        );
        policy::set_default(bench_policy());
        let app = TempApp::new(&source);
        // 新写的文件第一次打开可能被系统的安全扫描拦住，单独量。
        let started = Instant::now();
        std::hint::black_box(std::fs::read(app.0.join("main.js")).unwrap());
        let first_read = started.elapsed();
        let started = Instant::now();
        let loaded = runtime.load_application(&app.0, "main.js").unwrap();
        let load = started.elapsed();
        let rt = runtime.clone();
        let started = Instant::now();
        let window = cx.open_window(size(px(W), px(H)), move |window, cx| {
            let view = rt.mount_application(&loaded, window, cx).unwrap();
            gpui_base::Root::new(view, window, cx)
        });
        let mut context = VisualTestContext::from_window(*window.deref(), cx);
        draw(&mut context);
        println!(
            "JS   {:<30} 首次读文件 {:>6.1} ms  加载 {:>6.1} ms  挂载到第一帧 {:>6.1} ms  （{} KB 源码）",
            format!("启动 {lines} 行"),
            ms(first_read),
            ms(load),
            ms(started.elapsed()),
            source.len() / 1024
        );
    }
}
