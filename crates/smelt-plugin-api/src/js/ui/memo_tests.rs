//! gpui-shell 的 `memo(key, deps, build)`（vendor/gpui-shell 里标 SMELT: 的改动）：没变的段不再跑
//! 构建函数，拷过来的段照样能点、照样画对。

use gpui::{Modifiers, TestAppContext, VisualTestContext, point, px};

use super::tests::{draw, mount, tree};

fn click(context: &mut VisualTestContext, x: f32, y: f32) {
    context.simulate_click(point(px(x), px(y)), Modifiers::default());
    draw(context);
}

/// 五行，每行一个 memo；点最上面的条只改第三行。行按 key 记构建次数，显示在树里。
const ROWS: &str = r##"
import { View, div, memo } from "gpui-kit";
const builds = {};
export default class App extends View {
  init() { this.rows = ["a", "b", "c", "d", "e"]; this.clicks = 0; this.tick = 0; }
  render() {
    let col = div().flex().flex_col().absolute().top(20).left(0);
    for (const [i, text] of this.rows.entries()) {
      col = col.child(memo(`row:${i}`, [text], () => {
        builds[i] = (builds[i] ?? 0) + 1;
        return div().id(`row-${i}`).h(20).w(200).child(`row ${text}`)
          .on_click((_e, cx) => { this.clicks += 1; this.last = text; cx.notify(); });
      }));
    }
    return div().size_full()
      .child(div().id("bump").absolute().top(0).left(0).w(200).h(20)
        .on_click((_e, cx) => { this.rows[2] += "!"; cx.notify(); }))
      .child(div().id("tick").absolute().top(0).left(300).w(100).h(20)
        .on_click((_e, cx) => { this.tick += 1; cx.notify(); }))
      .child(col)
      .child(`builds: ${Object.entries(builds).map(([k, v]) => `${k}=${v}`).join(",")} clicks: ${this.clicks} last: ${this.last} tick: ${this.tick}`);
  }
}
"##;

#[gpui::test]
fn unchanged_rows_are_not_built_again(cx: &mut TestAppContext) {
    let (mut context, view, _app) = mount(cx, ROWS);
    draw(&mut context);
    let runtime = context.update(|_, cx| view.read(cx).runtime().clone());
    let before = runtime.read_metrics();
    click(&mut context, 8., 8.);
    let after = runtime.read_metrics().since(&before);
    let text = tree(&mut context, &view);
    assert!(text.contains("row c!"), "{text}");
    for row in ["row a", "row b", "row d", "row e"] {
        assert!(text.contains(row), "{row}: {text}");
    }
    // 树里的计数是在这一次构建里打出来的：第三行第二次构建，其余一次。
    assert!(text.contains("builds: 0=1,1=1,2=2,3=1,4=1 "), "{text}");
    assert_eq!(after.memo_hits(), 4);
    assert_eq!(after.memo_misses(), 1);
}

#[gpui::test]
fn reused_rows_keep_their_handlers(cx: &mut TestAppContext) {
    let (mut context, view, _app) = mount(cx, ROWS);
    draw(&mut context);
    // 几轮重建都沿用第一行：最早那一代描述早就收走了，处理函数得跟着新描述活着。
    for _ in 0..4 {
        click(&mut context, 308., 8.);
    }
    click(&mut context, 8., 28.);
    let text = tree(&mut context, &view);
    assert!(text.contains("clicks: 1 last: a tick: 4"), "{text}");
    assert!(text.contains("builds: 0=1,"), "row a built once: {text}");
    click(&mut context, 8., 48.);
    let text = tree(&mut context, &view);
    assert!(text.contains("clicks: 2 last: b"), "{text}");
}

/// 外层一块包着三行，外层的 deps 是三行的字。改外面的东西：外层整块沿用；之后改一行，外层重做，
/// 里面另外两行还得沿用——它们的记录要跟着外层命中那次搬进新描述。
const NESTED: &str = r##"
import { View, div, memo } from "gpui-kit";
const builds = {};
const count = (key) => { builds[key] = (builds[key] ?? 0) + 1; };
export default class App extends View {
  init() { this.rows = ["a", "b", "c"]; this.tick = 0; }
  render() {
    const block = memo("block", [...this.rows], () => {
      count("block");
      let col = div().flex().flex_col();
      for (const [i, text] of this.rows.entries()) {
        col = col.child(memo(`row:${i}`, [text], () => { count(i); return div().h(20).child(`row ${text}`); }));
      }
      return col;
    });
    return div().size_full()
      .child(div().id("tick").absolute().top(0).left(0).w(100).h(20)
        .on_click((_e, cx) => { this.tick += 1; cx.notify(); }))
      .child(div().id("bump").absolute().top(0).left(200).w(100).h(20)
        .on_click((_e, cx) => { this.rows[1] += "!"; cx.notify(); }))
      .child(div().absolute().top(40).child(block))
      .child(`builds: ${Object.entries(builds).map(([k, v]) => `${k}=${v}`).join(",")} tick: ${this.tick}`);
  }
}
"##;

#[gpui::test]
fn nested_records_follow_a_reused_block(cx: &mut TestAppContext) {
    let (mut context, view, _app) = mount(cx, NESTED);
    draw(&mut context);
    click(&mut context, 8., 8.);
    click(&mut context, 8., 8.);
    let text = tree(&mut context, &view);
    assert!(
        text.contains("builds: 0=1,1=1,2=1,block=1 tick: 2"),
        "{text}"
    );
    click(&mut context, 208., 8.);
    let text = tree(&mut context, &view);
    assert!(text.contains("row b!"), "{text}");
    assert!(text.contains("builds: 0=1,1=2,2=1,block=2 "), "{text}");
}

/// 注册组件的载荷里记着元素参数的位置（滚动区的顶上浮层）。前面多出几个节点，这一段整体后移，
/// 浮层得还是那个按钮，点得到。
const PAYLOAD: &str = r##"
import { View, div, memo } from "gpui-kit";
import { SmeltScrollView } from "smelt-ui";
export default class App extends View {
  init() { this.extra = 0; this.hits = 0; }
  render() {
    let root = div().size_full();
    for (let i = 0; i < this.extra; i++) root = root.child(div().absolute().top(0).left(0).w(1).h(1));
    const scroll = memo("scroll", [], () =>
      SmeltScrollView.new("scroll").absolute().top(40).left(0).w(300).h(200)
        .overlay_top(div().id("overlay").w(300).h(20)
          .on_click((_e, cx) => { this.hits += 1; cx.notify(); }))
        .child(div().h(400).child("content")));
    return root
      .child(div().id("grow").absolute().top(0).left(0).w(100).h(20)
        .on_click((_e, cx) => { this.extra += 3; cx.notify(); }))
      .child(scroll)
      .child(div().absolute().top(300).child(`extra: ${this.extra} hits: ${this.hits}`));
  }
}
"##;

#[gpui::test]
fn payload_elements_follow_a_moved_block(cx: &mut TestAppContext) {
    let (mut context, view, _app) = mount(cx, PAYLOAD);
    draw(&mut context);
    let runtime = context.update(|_, cx| view.read(cx).runtime().clone());
    let before = runtime.read_metrics();
    click(&mut context, 8., 8.);
    click(&mut context, 8., 8.);
    click(&mut context, 150., 48.);
    let text = tree(&mut context, &view);
    assert!(text.contains("extra: 6 hits: 1"), "{text}");
    assert!(runtime.read_metrics().since(&before).memo_hits() >= 2);
}

#[gpui::test]
fn a_key_used_twice_is_refused(cx: &mut TestAppContext) {
    let source = r##"
import { View, div, memo } from "gpui-kit";
export default class App extends View {
  render() {
    let message = "accepted";
    try {
      div().child(memo("same", [], () => div())).child(memo("same", [], () => div()));
    } catch (error) {
      message = String(error.message);
    }
    return div().child(message);
  }
}
"##;
    let (mut context, view, _app) = mount(cx, source);
    draw(&mut context);
    let text = tree(&mut context, &view);
    assert!(text.contains("is used twice in one render"), "{text}");
}

#[gpui::test]
fn a_block_using_outside_elements_is_built_every_time(cx: &mut TestAppContext) {
    let source = r##"
import { View, div, memo } from "gpui-kit";
let builds = 0;
export default class App extends View {
  init() { this.tick = 0; }
  render() {
    const outside = div().child(`outside ${this.tick}`);
    const block = memo("block", [], () => { builds += 1; return div().child(outside); });
    return div().size_full()
      .child(div().id("tick").absolute().top(0).left(0).w(100).h(20)
        .on_click((_e, cx) => { this.tick += 1; cx.notify(); }))
      .child(div().absolute().top(40).child(block))
      .child(`builds: ${builds}`);
  }
}
"##;
    let (mut context, view, _app) = mount(cx, source);
    draw(&mut context);
    click(&mut context, 8., 8.);
    let text = tree(&mut context, &view);
    assert!(text.contains("outside 1"), "{text}");
    assert!(text.contains("builds: 2"), "{text}");
}

/// 拿到 memo 的根以后接着加子元素、样式、处理函数：这些是外面的，每次都重新加，拷过来的段
/// 里不能带着上一次加的。
#[gpui::test]
fn the_root_takes_more_after_memo(cx: &mut TestAppContext) {
    let source = r##"
import { View, div, memo } from "gpui-kit";
let builds = 0;
export default class App extends View {
  init() { this.tick = 0; this.hits = 0; }
  render() {
    const tick = this.tick;
    const block = memo("block", [], () => { builds += 1; return div().w(200).child("inside"); })
      .h(20)
      .child(`outside ${tick}`)
      .on_click((_e, cx) => { this.hits += tick + 1; cx.notify(); });
    return div().size_full()
      .child(div().id("tick").absolute().top(0).left(0).w(100).h(20)
        .on_click((_e, cx) => { this.tick += 1; cx.notify(); }))
      .child(div().id("block").absolute().top(40).child(block))
      .child(`builds: ${builds} hits: ${this.hits}`);
  }
}
"##;
    let (mut context, view, _app) = mount(cx, source);
    draw(&mut context);
    click(&mut context, 8., 8.);
    click(&mut context, 8., 8.);
    let text = tree(&mut context, &view);
    assert_eq!(text.matches("outside").count(), 1, "{text}");
    assert!(text.contains("outside 2"), "{text}");
    assert_eq!(text.matches(":on_click").count(), 2, "{text}");
    assert!(text.contains("builds: 1 "), "{text}");
    // 处理函数是这一次的：tick 是 2，点一下加 3。
    click(&mut context, 8., 48.);
    let text = tree(&mut context, &view);
    assert!(text.contains("hits: 3"), "{text}");
}
