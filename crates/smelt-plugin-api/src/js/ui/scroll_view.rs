//! 竖着滚的一块内容（[`smelt_ui::scrollbar::Scrollable`] 配 [`smelt_ui::ScrollAnchors`]），比
//! `overflow_y_scrollbar()` 多认得里面的行：
//!
//! - `SmeltScrollAnchor.new(key)` 包住一行，滚动区域就记得它在哪。里面的吸顶区段
//!   （`SmeltStickySection`）停在顶上时，压在标题底下的行算滚出去了。
//! - `reveal(key, serial)`：头一回给、或 `serial` 变了，就把那一行滚到看得见（已经看得见不动，
//!   还没画出来就等它画出来）。同一行要再滚一次，换个 `serial`。
//! - `watch(keys)` 配 `on_offscreen((above, below, cx) => …)`：盯着的这几行里，滚到可视区上面、
//!   下面的各有哪些（按 `watch` 给的顺序），变了才报。
//! - `overlay_top(元素)` / `overlay_bottom(元素)`：浮在可视区上下沿的东西，上面那个让到停住的
//!   吸顶标题下面。
//! - `edges()`：滚过了上沿出一条细线，下面还有下沿出一条，和上下摆着的东西分开。
//! - `on_drop_files((paths, cx) => …)`：从访达拖文件进来松手，交来各个路径；拖在上面时铺一层
//!   选区色表示「放这里」。
//!
//! 里面拖着 `SmeltDragItem` 贴着上下边缘时自动滚，离开边缘、松手就停。
//!
//! 子元素是内容，样式落在外框上（`flex_1`、`min_h_0` 这些让它占满剩下的高度）。

use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Context, ElementId, EntityId, ExternalPaths, InteractiveElement as _,
    IntoElement, ParentElement as _, Pixels, Point, RenderOnce, ScrollHandle, StyleRefinement,
    Styled as _, Window, div, point, px,
};
use gpui_base::{AutoScroll, StyledExt as _};
use gpui_shell::{
    ArgumentDescriptor, ArgumentSchema, ComponentArgument, ComponentCallback,
    ComponentCallbackArgument, ComponentDescriptor, ComponentMaterializer, ComponentPayload,
    ComponentRegistry, ConstructorDescriptor, MaterializeRequest, MethodDescriptor, RegistryError,
    anyhow, gpui,
};
use smelt_ui::scrollbar::{Scrollable, ScrollbarAxis};
use smelt_ui::{Placement, ScrollAnchors, ui_theme};

use super::drag_item::ItemDrag;
use super::{callback_method, control_id, flag_method, id_constructor, ops};

const VIEW: &str = "SmeltScrollView";
const ANCHOR: &str = "SmeltScrollAnchor";

#[derive(Clone)]
enum Op {
    Reveal(String, f64),
    Watch(Vec<String>),
    OnOffscreen(ComponentArgument),
    OverlayTop(ComponentArgument),
    OverlayBottom(ComponentArgument),
    Edges,
    OnDropFiles(ComponentArgument),
}

/// 跨帧留着的：滚到哪、各行在哪，上次按过的 `serial`，上次报过的上下两边。
struct State {
    scroll: ScrollHandle,
    anchors: ScrollAnchors,
    revealed: Option<f64>,
    reported: (Vec<String>, Vec<String>),
    auto_scroll: AutoScroll,
    /// 画这块滚动区域的视图：自动滚了要它重画。
    view: Option<EntityId>,
}

#[derive(IntoElement)]
struct ScrollView {
    id: ElementId,
    content: Vec<AnyElement>,
    style: StyleRefinement,
    reveal: Option<(String, f64)>,
    watch: Vec<String>,
    on_offscreen: Option<ComponentCallback>,
    overlay_top: Option<AnyElement>,
    overlay_bottom: Option<AnyElement>,
    edges: bool,
    on_drop_files: Option<ComponentCallback>,
}

impl RenderOnce for ScrollView {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(self.id.clone(), cx, |_, _| {
            let scroll = ScrollHandle::new();
            State {
                anchors: ScrollAnchors::new(&scroll),
                scroll,
                revealed: None,
                reported: Default::default(),
                auto_scroll: AutoScroll::default(),
                view: None,
            }
        });
        let view = window.current_view();
        state.update(cx, |state, _| state.view = Some(view));
        let (scroll, anchors) = {
            let state = state.read(cx);
            (state.scroll.clone(), state.anchors.clone())
        };
        if let Some((key, serial)) = self.reveal
            && state.read(cx).revealed != Some(serial)
        {
            state.update(cx, |state, _| state.revealed = Some(serial));
            anchors.reveal(Some(key.into()));
        }
        if let Some(callback) = self.on_offscreen {
            // 位置是上一帧画出来的，配眼下的滚动位置：滚动、位置变了都会重画到这里。
            let (mut above, mut below) = (Vec::new(), Vec::new());
            for key in self.watch {
                match anchors.placement(&key) {
                    Some(Placement::Above(_)) => above.push(key),
                    Some(Placement::Below(_)) => below.push(key),
                    Some(Placement::Visible) | None => {}
                }
            }
            let sides = (above, below);
            if state.read(cx).reported != sides {
                state.update(cx, |state, _| state.reported = sides.clone());
                let list = |keys: Vec<String>| {
                    ComponentCallbackArgument::Array(
                        keys.into_iter()
                            .map(ComponentCallbackArgument::String)
                            .collect(),
                    )
                };
                let args = [list(sides.0), list(sides.1)];
                window.defer(cx, move |window, cx| {
                    callback.invoke_and_report_with(
                        "SmeltScrollView.on_offscreen callback failed",
                        &args,
                        window,
                        cx,
                    );
                });
            }
        }
        let offset = scroll.offset().y;
        let max = scroll.max_offset().y;
        let edge = |shown: bool| {
            // 线一直占着 1px，只换颜色，出现时不跳。
            div()
                .flex_none()
                .h(px(1.))
                .when(shown, |line| line.bg(ui_theme::hair()))
        };
        let content = div()
            .flex()
            .flex_col()
            .child(anchors.root(div().flex().flex_col().children(self.content)));
        let inset = anchors.top_inset();
        div()
            .flex()
            .flex_col()
            .min_h_0()
            .refine_style(&self.style)
            .when(self.edges, |frame| frame.child(edge(offset < px(0.))))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .on_drag_move::<ItemDrag>({
                        let state = state.downgrade();
                        move |event, _, cx| {
                            let _ = state.update(cx, |state, cx| {
                                auto_scroll(state, event.event.position, cx)
                            });
                        }
                    })
                    .when_some(self.on_drop_files, |area, callback| {
                        area.drag_over::<ExternalPaths>(|style, _, _, _| {
                            style.bg(ui_theme::selection())
                        })
                        .on_drop(
                            move |paths: &ExternalPaths, window, cx| {
                                let paths = paths
                                    .paths()
                                    .iter()
                                    .map(|path| {
                                        ComponentCallbackArgument::String(
                                            path.to_string_lossy().into_owned(),
                                        )
                                    })
                                    .collect();
                                callback.invoke_and_report_with(
                                    "SmeltScrollView.on_drop_files callback failed",
                                    &[ComponentCallbackArgument::Array(paths)],
                                    window,
                                    cx,
                                );
                            },
                        )
                    })
                    .child(Scrollable::new(
                        (self.id.clone(), "scroll"),
                        content,
                        &scroll,
                        ScrollbarAxis::Vertical,
                    ))
                    .when_some(self.overlay_top, |area, overlay| {
                        area.child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .top(inset)
                                .child(overlay),
                        )
                    })
                    .when_some(self.overlay_bottom, |area, overlay| {
                        area.child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .bottom_0()
                                .child(overlay),
                        )
                    }),
            )
            .when(self.edges, |frame| frame.child(edge(offset > -max)))
    }
}

/// 拖着走时指针动了：贴着上下边缘就开始（或接着）自动滚，离开边缘就停。
fn auto_scroll(state: &mut State, position: Point<Pixels>, cx: &mut Context<State>) {
    let bounds = state.scroll.bounds();
    let beside = position.x >= bounds.left() && position.x <= bounds.right();
    let delta = beside
        .then(|| AutoScroll::compute_delta(position.y, bounds))
        .flatten();
    // 轮询例外：指针停在边缘不动就没有事件，滚动只能按帧推进；周期 16ms，只在拖着且停在
    // 上下边缘时跑，离开边缘、松手（下一帧看到没在拖）就停。
    state.auto_scroll.set(delta, cx, |delta, state, cx| {
        if !cx.has_active_drag() {
            state.auto_scroll.stop();
            return;
        }
        let offset = state.scroll.offset();
        let max = f32::from(state.scroll.max_offset().y);
        let next = px((f32::from(offset.y) - f32::from(delta)).clamp(-max, 0.));
        if next != offset.y {
            state.scroll.set_offset(point(offset.x, next));
            if let Some(view) = state.view {
                App::notify(cx, view);
            }
        }
    });
}

struct ScrollViewMaterializer;

impl ComponentMaterializer for ScrollViewMaterializer {
    fn materialize(&self, mut request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        let id = control_id(VIEW, &request)?;
        let mut view = ScrollView {
            id: id.into(),
            content: Vec::new(),
            style: StyleRefinement::default(),
            reveal: None,
            watch: Vec::new(),
            on_offscreen: None,
            overlay_top: None,
            overlay_bottom: None,
            edges: false,
            on_drop_files: None,
        };
        for op in ops::<Op>(&request).cloned().collect::<Vec<_>>() {
            match op {
                Op::Reveal(key, serial) => view.reveal = Some((key, serial)),
                Op::Watch(keys) => view.watch = keys,
                Op::OnOffscreen(callback) => {
                    view.on_offscreen = Some(request.resolve_callback(&callback)?)
                }
                Op::OverlayTop(element) => {
                    view.overlay_top = Some(request.resolve_element(&element)?)
                }
                Op::OverlayBottom(element) => {
                    view.overlay_bottom = Some(request.resolve_element(&element)?)
                }
                Op::Edges => view.edges = true,
                Op::OnDropFiles(callback) => {
                    view.on_drop_files = Some(request.resolve_callback(&callback)?)
                }
            }
        }
        anyhow::ensure!(
            view.on_offscreen.is_some() || view.watch.is_empty(),
            "{VIEW}.watch needs on_offscreen to report to"
        );
        view.style = request.take_style();
        view.content = request.take_children()?;
        Ok(view.into_any_element())
    }
}

struct AnchorMaterializer;

impl ComponentMaterializer for AnchorMaterializer {
    fn materialize(&self, request: MaterializeRequest<'_>) -> anyhow::Result<gpui::AnyElement> {
        let key = request
            .payload()
            .downcast_ref::<AnchorKey>()
            .ok_or_else(|| anyhow::anyhow!("{ANCHOR} received an incompatible payload"))?
            .0
            .clone();
        request.finish(ScrollAnchors::anchor_enclosing(key, div()))
    }
}

#[derive(Clone)]
struct AnchorKey(String);

fn element_method(
    name: &'static str,
    documentation: &'static str,
    op: fn(ComponentArgument) -> Op,
) -> MethodDescriptor {
    MethodDescriptor::new(
        name,
        vec![ArgumentDescriptor::new("element", ArgumentSchema::Element)],
        move |args| match args {
            [element @ ComponentArgument::Element(_)] => {
                Ok(ComponentPayload::new(op(element.clone())))
            }
            _ => Err(format!("{VIEW}.{name}(element) expects an element")),
        },
    )
    .with_documentation(documentation)
}

pub(super) fn register(registry: &mut ComponentRegistry) -> Result<(), RegistryError> {
    let reveal = MethodDescriptor::new(
        "reveal",
        vec![
            ArgumentDescriptor::new("key", ArgumentSchema::String),
            ArgumentDescriptor::new("serial", ArgumentSchema::Number),
        ],
        |args| match args {
            [ComponentArgument::String(key), ComponentArgument::Number(serial)] => {
                Ok(ComponentPayload::new(Op::Reveal(key.clone(), *serial)))
            }
            _ => Err(format!("{VIEW}.reveal(key, serial) expects a string and a number")),
        },
    )
    .with_documentation(
        "Scrolls the SmeltScrollAnchor with this key into view the first time it is given and whenever serial changes; waits for it to be drawn.",
    );
    let watch = MethodDescriptor::new(
        "watch",
        vec![ArgumentDescriptor::new(
            "keys",
            ArgumentSchema::Array(Box::new(ArgumentSchema::String)),
        )],
        |args| match args {
            [ComponentArgument::Array(keys)] => keys
                .iter()
                .map(|key| match key {
                    ComponentArgument::String(key) => Ok(key.clone()),
                    _ => Err(format!("{VIEW}.watch(keys) expects strings")),
                })
                .collect::<Result<Vec<_>, _>>()
                .map(|keys| ComponentPayload::new(Op::Watch(keys))),
            _ => Err(format!("{VIEW}.watch(keys) expects an array of strings")),
        },
    )
    .with_documentation(
        "Anchor keys whose position on_offscreen reports, in the order the reports list them.",
    );
    registry.register(
        ComponentDescriptor::new(VIEW, Arc::new(ScrollViewMaterializer))
            .with_constructors(vec![id_constructor(VIEW)])
            .with_methods(vec![
                reveal,
                watch,
                callback_method(
                    VIEW,
                    "on_offscreen",
                    "(above: string[], below: string[], cx: Context) => void",
                    "Runs when the watched anchors scrolled above or below the visible area change (rows under a stuck sticky header count as above).",
                    Op::OnOffscreen,
                ),
                element_method(
                    "overlay_top",
                    "Floats over the top edge of the visible area, below any stuck sticky header.",
                    Op::OverlayTop,
                ),
                element_method(
                    "overlay_bottom",
                    "Floats over the bottom edge of the visible area.",
                    Op::OverlayBottom,
                ),
                callback_method(
                    VIEW,
                    "on_drop_files",
                    "(paths: string[], cx: Context) => void",
                    "Runs when files or folders dragged in from the system are dropped on the area, with their paths.",
                    Op::OnDropFiles,
                ),
                flag_method(
                    "edges",
                    Op::Edges,
                    "Shows a hairline at the top once scrolled, and at the bottom while more is below.",
                ),
            ])
            .with_documentation(
                "A vertical scroll area with the Smelt scrollbar that knows its SmeltScrollAnchor rows: reveal, watch, overlays; styles apply to the frame.",
            ),
    )?;
    registry.register(
        ComponentDescriptor::new(ANCHOR, Arc::new(AnchorMaterializer))
            .with_constructors(vec![ConstructorDescriptor::new(
                ANCHOR,
                vec![ArgumentDescriptor::new("key", ArgumentSchema::String)],
                |args| match args {
                    [ComponentArgument::String(key)] if !key.is_empty() => {
                        Ok(ComponentPayload::new(AnchorKey(key.clone())))
                    }
                    _ => Err(format!("{ANCHOR}.new(key) expects a non-empty string")),
                },
            )])
            .with_documentation(
                "Marks its children as one row of the enclosing SmeltScrollView, by key (unique within the view).",
            ),
    )?;
    Ok(())
}
