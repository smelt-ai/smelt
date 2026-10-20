//! markdown 渲染里的 mermaid 代码块画图：全仓库唯一入口。
//!
//! `gpui-component` 的 `TextView::markdown` 默认把 mermaid 围栏代码块当普通代码
//! 块处理（纯文本 + 语法高亮壳，mermaid 语法本身没有 tree-sitter 语法，等于纯文
//! 本）。这里接上它的 `markdown_block_parser`/`markdown_block_renderer` 钩子，识
//! 别出 mermaid 代码块，用 `merman` 0.9.1（纯 Rust，对齐 mermaid.js，Zed 也在用）
//! 离线渲成 SVG，再用 GPUI 的 `img()` 全彩显示——注意不是 `svg()`：那个元素只做
//! 单色 alpha mask 渲染（图标那条路），画不出多彩图，`img()` 对 `.svg` 走的才是
//! 全彩栅格化管线。
//!
//! 渲染库换过两次。最早是 `mermaid-rs-renderer`，反向边绕路。接着换成
//! `rusty-mermaid` 0.2.0，布局好一些，但中文子图 id 解析失败，中文状态名会在
//! `state/parser.rs:70` 按字节切 UTF-8 直接 panic，stateDiagram 布局不对，ER 注释
//! 被拼进属性名，也不认识 `@{ shape }`。`merman` 按 mermaid.js 做对齐，37 种图，
//! 就是为了换掉这些问题。不保留旧渲染器的开关或垫片。
//!
//! 输出背景是透明的，跟着外面的亮暗主题走；节点配色仍由 merman 的 theme 决定，
//! 所以缓存 key 还是要把主题模式编进去。字体写进站点配置的 `fontFamily`，不再
//! 在 SVG 文本里做字符串替换。类图标签包在带 `overflow:hidden` 的嵌套 svg 里，
//! 测宽回调如果只按字符数估算，文字会被裁掉；这里用 GPUI 同一份 usvg 0.46 的
//! 系统 fontdb，再用 rustybuzz 量 advance。
//!
//! 不编 `layout-elk`。它带进的 `merman-elk-layered` 是 EPL-2.0 的 ELK 移植，链进二进制
//! 就得随附 EPL 文本、出处和对应源码，仓库没有这套流程。而且上游
//! `default_config.json` 根上的 `layout` 默认是 `elk`，编进来后所有没写布局的流程图、
//! 状态图、类图、ER 都会换成 ELK，和 GitHub / mermaid.js 上看到的排法不一样。
//! 不编这个 feature 时，`elk` 请求缺 loader，退回 dagre；图里写 `layout: elk` 也一样。
//!
//! 全仓库新增/已有的 `TextView::markdown` 调用点都应该改走这里的 `markdown_view`，
//! 不要直接调 `TextView::markdown`，否则等于每处都得重新接一遍这两个钩子。
//!
//! 内存缓存：GPUI 是即时模式 UI，`BlockNode` 的渲染函数每帧 paint 都会重新调用
//! （对照 gpui-component `node.rs` 里 `CODE_BLOCK_HIGHLIGHTERS` 那个 thread_local
//! 先例），同一张图只渲一次，否则滚动会变成每帧重算。不落盘——这是派生图，不是状态。

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, AvailableSpace, Bounds, Context, DevicePixels, DragMoveEvent,
    Element, ElementId, EmptyView, GlobalElementId, HighlightStyle, ImageSource,
    InspectorElementId, InteractiveElement, IntoElement, LayoutId, MouseButton, MouseDownEvent,
    ObjectFit, ParentElement, Pixels, Point, Render, RenderImage, ScrollHandle, SharedString, Size,
    StatefulInteractiveElement, Style, Styled, StyledImage, SvgSize, Task, Window, canvas, div,
    img, point, px, rgb, size,
};
use gpui_component::scroll::{ScrollableElement as _, ScrollbarAxis};
use gpui_component::text::{
    MarkdownExtensions, MarkdownNode, MarkdownParseContext, TextView, TextViewState, TextViewStyle,
    markdown_ast,
};
use gpui_component::{ActiveTheme, WindowExt as _, h_flex, v_flex};
use merman::svg::{
    DeterministicTextMeasurer, MeasurementProfileId, SvgPipeline, TextMeasurementPolicy,
    TextMeasurementProfile, TextMeasurementProfileIdentity, TextStyle,
};
use merman::{
    Engine, MermaidConfig, OperationControl, RenderOutput, RenderRequest, Renderer, SvgEnvironment,
    SvgRequest,
};
use sha2::{Digest, Sha256};

use crate::IconAction as _;

/// 传给 merman `fontFamily` 的 CSS 字体栈。多词名字必须加引号，否则 CSS 会把
/// `Hiragino Sans GB` 拆成三个 family。GPUI 的 usvg 按字面名精确匹配
/// `font-family`，不认 `sans-serif` 这类泛型关键字，栈里得有一个系统字体目录里
/// 真能查到的名字，中英文才画得出来。
///
/// 首选没直接写 PingFang SC：它的文件在
/// `/System/Library/PrivateFrameworks/FontServices.framework/.../Reserved/
/// PingFangUI.ttc`，fontdb 只扫标准目录，任何拼法都查不到。冬青黑体简体中文
/// （Hiragino Sans GB）是 PingFang 之前 macOS 用了很多年的中文默认字体，笔画比
/// 黑体-简细，而且在标准字体目录里。
const MERMAID_FONT_FAMILY: &str =
    "\"Hiragino Sans GB\", \"Heiti SC\", \"PingFang SC\", \"Helvetica Neue\", Arial, sans-serif";

/// 机器上没有上面这套字体时，测宽再试的 CJK 家族。只参与量字宽，不写进 SVG 的
/// `font-family`：macOS 上 Hiragino 能命中，GPUI 栅格化走的就是这条栈。
const CJK_MEASURE_FALLBACKS: &[&str] = &[
    "Noto Sans CJK SC",
    "Noto Sans CJK JP",
    "Noto Sans SC",
    "Source Han Sans SC",
    "WenQuanYi Micro Hei",
    "Droid Sans Fallback",
];

/// 内存缓存 key 版本：换了渲染库、字体栈或 SVG 后处理就跟着升，避免同进程里旧产物被复用。
const CACHE_FORMAT_VERSION: &str = "v5";

/// 粗体没有独立字重时，在 regular advance 上乘的系数。跟评估原型一致。
const SYNTHETIC_BOLD_SCALE: f64 = 1.06;

/// parser 阶段存进 `MarkdownNode` 的原始数据。parser 不能碰 Window/App，真正的
/// 渲染（调 merman、查缓存）都留到 renderer 阶段做。
struct MermaidBlockData {
    source: SharedString,
}

/// 判断一个 mdast 节点是不是 ```mermaid 围栏代码块，是的话取出源码。跟
/// `parse_mermaid_block` 分开是因为 `MarkdownParseContext` 的构造函数对 smelt
/// 不可见（`pub(crate)`），这个纯函数不需要 context 就能单测。
fn mermaid_source(node: &markdown_ast::Node) -> Option<SharedString> {
    let markdown_ast::Node::Code(code) = node else {
        return None;
    };
    if code.lang.as_deref() != Some("mermaid") {
        return None;
    }
    Some(code.value.clone().into())
}

/// 识别 ```mermaid 围栏代码块。命中就短路掉 gpui-component 默认的 CodeBlock 处理
/// （见 `format/markdown.rs` 里 `parse_block` 先跑自定义 parser 再走默认分支）。
fn parse_mermaid_block(
    node: &markdown_ast::Node,
    _cx: &MarkdownParseContext<'_>,
) -> Option<MarkdownNode> {
    let source = mermaid_source(node)?;
    Some(
        MarkdownNode::new(
            "mermaid",
            MermaidBlockData {
                source: source.clone(),
            },
        )
        .text(source.clone())
        .markdown(format!("```mermaid\n{source}\n```")),
    )
}

/// 渲染成功后要显示的东西：full-color SVG 的字节数据 + 它自身的自然像素尺寸
/// （从 SVG 的 `viewBox` 读出来）。内嵌时按列宽缩小、不放大（见 [`MermaidInline`]），
/// 看细节点开放大预览（见 [`MermaidPreview`]）。
struct MermaidImage {
    image: Arc<gpui::Image>,
    width: f32,
    height: f32,
}

/// 内嵌尺寸：放进可用宽度，不超过自然尺寸，高度按比例跟着宽度走。
fn mermaid_inline_size(width: f32, height: f32, available_width: Option<f32>) -> Size<Pixels> {
    let scale = available_width.map_or(1.0, |available| (available / width).clamp(0.0, 1.0));
    size(px(width * scale), px(height * scale))
}

/// 内嵌的 mermaid 图。
///
/// 复杂图的自然尺寸远超对话列宽，原尺寸加横向滚动只能露出一角。这里用测量布局：
/// taffy 给出可用宽度时直接算出宽高，不靠 `aspect_ratio` 推导（`crate::image`
/// 模块注释记着那是历次图片撑破、留白的根因），也不靠上一帧量到的列宽。
struct MermaidInline {
    image: Arc<MermaidImage>,
}

impl IntoElement for MermaidInline {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for MermaidInline {
    type RequestLayoutState = ();
    type PrepaintState = AnyElement;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        _cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let (width, height) = (self.image.width, self.image.height);
        let layout_id =
            window.request_measured_layout(Style::default(), move |known, available, _, _| {
                let available_width = known.width.or(match available.width {
                    AvailableSpace::Definite(width) => Some(width),
                    // 最小内容宽度给 0，所在的 flex 行才能把它压到列宽以内。
                    AvailableSpace::MinContent => Some(px(0.)),
                    AvailableSpace::MaxContent => None,
                });
                mermaid_inline_size(width, height, available_width.map(f32::from))
            });
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        // 父级若把盒子拉宽了，图仍按自己的比例靠左画，不拉伸。
        let fitted = mermaid_inline_size(
            self.image.width,
            self.image.height,
            Some(f32::from(bounds.size.width)),
        );
        let mut image = img(self.image.image.clone())
            .w(fitted.width)
            .h(fitted.height)
            .into_any_element();
        image.prepaint_as_root(
            bounds.origin,
            size(
                AvailableSpace::Definite(fitted.width),
                AvailableSpace::Definite(fitted.height),
            ),
            window,
            cx,
        );
        image
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        image: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        image.paint(window, cx);
    }
}

/// 预览缩放倍数都相对 SVG 自然尺寸（1.0 = 内嵌时的大小）。
const MIN_PREVIEW_SCALE: f32 = 0.1;
const MAX_PREVIEW_SCALE: f32 = 8.0;
const PREVIEW_ZOOM_STEP: f32 = 1.25;

/// 预览栅格的上限。gpui sprite atlas 单图上限 16384px；像素总数再压一道，
/// 大图放到最大倍数时一张 RGBA 不超过 64MB。超过上限就拉伸这张封顶的栅格。
const MAX_RASTER_SIDE: f32 = 8192.0;
const MAX_RASTER_PIXELS: f32 = 16_777_216.0;

fn preview_zoom_in(scale: f32) -> f32 {
    (scale * PREVIEW_ZOOM_STEP).min(MAX_PREVIEW_SCALE)
}

fn preview_zoom_out(scale: f32) -> f32 {
    (scale / PREVIEW_ZOOM_STEP).max(MIN_PREVIEW_SCALE)
}

/// 「适合窗口」时的实际倍数。小图也会放大填满：预览重新栅格化，放大不糊。
fn preview_fit_scale(image_width: f32, image_height: f32, viewport: Size<Pixels>) -> f32 {
    (f32::from(viewport.width) / image_width).min(f32::from(viewport.height) / image_height)
}

/// 缩放后单个轴上的滚动偏移：保持视口中心对着图上同一点。图比视口窄时它是居中
/// 摆放的，中心就是图的中点。结果夹在可滚动范围内（偏移向内容方向为负）。
fn preview_anchored_offset(offset: f32, viewport: f32, old_len: f32, new_len: f32) -> f32 {
    let center = if old_len > viewport {
        (viewport / 2.0 - offset) / old_len
    } else {
        0.5
    };
    let max_scroll = (new_len - viewport).max(0.0);
    (viewport / 2.0 - center * new_len).clamp(-max_scroll, 0.0)
}

fn preview_pan_offset(
    scroll_origin: Point<Pixels>,
    pointer_origin: Point<Pixels>,
    pointer_current: Point<Pixels>,
) -> Point<Pixels> {
    scroll_origin + pointer_current - pointer_origin
}

/// 按设备像素算预览栅格尺寸，超过上限时等比缩到上限以内。
fn preview_raster_size(width: f32, height: f32, device_scale: f32) -> Size<DevicePixels> {
    let w = width * device_scale;
    let h = height * device_scale;
    let shrink = (MAX_RASTER_SIDE / w.max(h))
        .min((MAX_RASTER_PIXELS / (w * h)).sqrt())
        .min(1.0);
    size(
        DevicePixels(((w * shrink).ceil() as i32).max(1)),
        DevicePixels(((h * shrink).ceil() as i32).max(1)),
    )
}

/// 视口尺寸的小抖动不值得重新栅格化一次。
fn raster_close_enough(a: Size<DevicePixels>, b: Size<DevicePixels>) -> bool {
    let close = |a: i32, b: i32| (a - b).abs() <= (b / 50).max(2);
    close(a.width.0, b.width.0) && close(a.height.0, b.height.0)
}

#[derive(Clone, Copy, PartialEq)]
enum PreviewZoom {
    /// 交给布局按 contain 放进视口，窗口变了跟着变。
    Fit,
    Scale(f32),
}

/// 拖拽平移的载荷。只用来让 `on_drag_move` 在指针离开预览区后还能收到移动。
struct MermaidPan;

/// 放大预览。
///
/// `gpui::Image` 的 SVG 只按自然尺寸的 2 倍栅格化一次，放大就是拉伸位图，所以
/// 预览按当前倍数和窗口缩放自己在后台重新栅格化，栅格好之前先拉伸原图顶着。
/// 视口尺寸来自上一帧的布局（`measure`），render 只读这些缓存。
struct MermaidPreview {
    image: Arc<MermaidImage>,
    zoom: PreviewZoom,
    scroll: ScrollHandle,
    viewport: Option<Size<Pixels>>,
    scale_factor: f32,
    raster: Option<Arc<RenderImage>>,
    /// 在途栅格的目标尺寸，避免同一尺寸重复起任务。
    pending_raster: Option<Size<DevicePixels>>,
    raster_task: Option<Task<()>>,
    /// 按下左键时的指针位置和滚动偏移；内容可滚动时才记。
    pan_origin: Option<(Point<Pixels>, Point<Pixels>)>,
}

impl MermaidPreview {
    fn new(image: Arc<MermaidImage>, cx: &mut Context<Self>) -> Self {
        // `RenderImage` 的纹理按 id 留在 sprite atlas 里，只丢 `Arc` 不会释放。
        cx.on_release(|this, cx| {
            if let Some(raster) = this.raster.take() {
                cx.drop_image(raster, None);
            }
        })
        .detach();
        Self {
            image,
            zoom: PreviewZoom::Fit,
            scroll: ScrollHandle::new(),
            viewport: None,
            scale_factor: 1.0,
            raster: None,
            pending_raster: None,
            raster_task: None,
            pan_origin: None,
        }
    }

    fn effective_scale(&self) -> Option<f32> {
        match self.zoom {
            PreviewZoom::Scale(scale) => Some(scale),
            PreviewZoom::Fit => self
                .viewport
                .map(|viewport| preview_fit_scale(self.image.width, self.image.height, viewport)),
        }
    }

    /// 预览区 prepaint 时回报自己的尺寸。只有变了才重新栅格化、重绘。
    fn measure(&mut self, viewport: Size<Pixels>, scale_factor: f32, cx: &mut Context<Self>) {
        if viewport.width <= px(0.) || viewport.height <= px(0.) {
            return;
        }
        if self.viewport == Some(viewport) && self.scale_factor == scale_factor {
            return;
        }
        self.viewport = Some(viewport);
        self.scale_factor = scale_factor;
        self.sync_raster(cx);
        cx.notify();
    }

    fn sync_raster(&mut self, cx: &mut Context<Self>) {
        let Some(scale) = self.effective_scale() else {
            return;
        };
        let target = preview_raster_size(
            self.image.width,
            self.image.height,
            scale * self.scale_factor,
        );
        let current = self
            .pending_raster
            .or_else(|| self.raster.as_ref().map(|raster| raster.size(0)));
        if current.is_some_and(|current| raster_close_enough(current, target)) {
            return;
        }
        self.pending_raster = Some(target);
        let image = self.image.image.clone();
        let renderer = cx.svg_renderer();
        self.raster_task = Some(cx.spawn(async move |this, cx| {
            let raster = cx
                .background_executor()
                .spawn(async move {
                    renderer
                        .parse_svg(image.bytes())
                        .and_then(|svg| renderer.render_parsed(&svg, SvgSize::ExactSize(target)))
                })
                .await;
            this.update(cx, |this, cx| {
                this.pending_raster = None;
                // 失败就继续拉伸原图：内嵌时这张 SVG 已经画出来过。
                let Ok(raster) = raster else {
                    return;
                };
                if let Some(old) = this.raster.replace(raster) {
                    cx.drop_image(old, None);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn zoom_to(&mut self, next: f32, cx: &mut Context<Self>) {
        let current = self.effective_scale().unwrap_or(1.0);
        if let Some(viewport) = self.viewport {
            let offset = self.scroll.offset();
            let (width, height) = (self.image.width, self.image.height);
            self.scroll.set_offset(point(
                px(preview_anchored_offset(
                    f32::from(offset.x),
                    f32::from(viewport.width),
                    width * current,
                    width * next,
                )),
                px(preview_anchored_offset(
                    f32::from(offset.y),
                    f32::from(viewport.height),
                    height * current,
                    height * next,
                )),
            ));
        }
        self.zoom = PreviewZoom::Scale(next);
        self.sync_raster(cx);
        cx.notify();
    }

    fn zoom_fit(&mut self, cx: &mut Context<Self>) {
        self.zoom = PreviewZoom::Fit;
        self.scroll.set_offset(point(px(0.), px(0.)));
        self.sync_raster(cx);
        cx.notify();
    }
}

impl Render for MermaidPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let scale = self.effective_scale();
        let source: ImageSource = match &self.raster {
            Some(raster) => raster.clone().into(),
            None => self.image.image.clone().into(),
        };
        let content = match (self.zoom, self.viewport) {
            (PreviewZoom::Scale(scale), Some(viewport)) => {
                let width = self.image.width * scale;
                let height = self.image.height * scale;
                // 图比视口小的那个轴撑满视口并居中，大的那个轴才可滚动。
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(width.max(f32::from(viewport.width))))
                    .h(px(height.max(f32::from(viewport.height))))
                    .child(
                        img(source)
                            .flex_none()
                            .w(px(width))
                            .h(px(height))
                            .object_fit(ObjectFit::Fill),
                    )
                    .into_any_element()
            }
            _ => img(source)
                .size_full()
                .object_fit(ObjectFit::Contain)
                .into_any_element(),
        };

        let viewport = div()
            .id("mermaid-preview-viewport")
            .size_full()
            .overflow_scroll()
            .track_scroll(&self.scroll)
            .when(matches!(self.zoom, PreviewZoom::Scale(_)), |this| {
                this.cursor_grab()
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, _| {
                    let max = this.scroll.max_offset();
                    this.pan_origin = (max.x > px(0.) || max.y > px(0.))
                        .then(|| (event.position, this.scroll.offset()));
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.pan_origin = None),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.pan_origin = None),
            )
            .on_drag(MermaidPan, |_, _, _, cx| cx.new(|_| EmptyView))
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<MermaidPan>, _, cx| {
                    let Some((pointer_origin, scroll_origin)) = this.pan_origin else {
                        return;
                    };
                    this.scroll.set_offset(preview_pan_offset(
                        scroll_origin,
                        pointer_origin,
                        event.event.position,
                    ));
                    cx.notify();
                }),
            )
            .child(content)
            .scrollbar(&self.scroll, ScrollbarAxis::Both);

        let measure = canvas(
            {
                let view = cx.entity();
                move |bounds, window, cx| {
                    let scale_factor = window.scale_factor();
                    view.update(cx, |this, cx| this.measure(bounds.size, scale_factor, cx));
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0();

        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .gap_2()
            .child(
                h_flex()
                    .justify_end()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .children(scale.map(|scale| format!("{:.0}%", scale * 100.0))),
                    )
                    .child(
                        crate::Button::new("mermaid-preview-zoom-out")
                            .quiet()
                            .compact()
                            .label("缩小")
                            .debug_selector(|| "MERMAID_PREVIEW_ZOOM_OUT".to_string())
                            .disabled(scale.is_some_and(|scale| scale <= MIN_PREVIEW_SCALE))
                            .on_click(cx.listener(|this, _, _, cx| {
                                let scale = this.effective_scale().unwrap_or(1.0);
                                this.zoom_to(preview_zoom_out(scale), cx);
                            })),
                    )
                    .child(
                        crate::Button::new("mermaid-preview-zoom-in")
                            .quiet()
                            .compact()
                            .label("放大")
                            .debug_selector(|| "MERMAID_PREVIEW_ZOOM_IN".to_string())
                            .disabled(scale.is_some_and(|scale| scale >= MAX_PREVIEW_SCALE))
                            .on_click(cx.listener(|this, _, _, cx| {
                                let scale = this.effective_scale().unwrap_or(1.0);
                                this.zoom_to(preview_zoom_in(scale), cx);
                            })),
                    )
                    .child(
                        crate::Button::new("mermaid-preview-zoom-fit")
                            .quiet()
                            .compact()
                            .label("适合窗口")
                            .debug_selector(|| "MERMAID_PREVIEW_ZOOM_FIT".to_string())
                            .disabled(self.zoom == PreviewZoom::Fit)
                            .on_click(cx.listener(|this, _, _, cx| this.zoom_fit(cx))),
                    ),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .child(viewport)
                    .child(measure),
            )
    }
}

fn open_mermaid_preview(image: Arc<MermaidImage>, window: &mut Window, cx: &mut App) {
    let preview = cx.new(|cx| MermaidPreview::new(image, cx));
    window.open_dialog(cx, move |dialog, window, _cx| {
        // 对话框会把宽高夹在窗口减去边距以内，这里只表达「尽量大」，不替它算边距。
        let viewport = window.viewport_size();
        let preview = preview.clone();
        dialog
            .title("Mermaid 图表")
            .width(viewport.width)
            .h(viewport.height)
            .content(move |content, _, _| content.min_h_0().child(preview.clone()))
    });
}

#[derive(Clone)]
enum MermaidRender {
    Ok(Arc<MermaidImage>),
    Err(Arc<str>),
}

thread_local! {
    /// 内存态一级缓存，key 是 `source_digest`（已经把主题模式编进去了）。
    static MERMAID_RENDER_CACHE: RefCell<HashMap<String, MermaidRender>> = RefCell::new(HashMap::new());
    /// 正在后台渲染的 digest 集合：同一张图只起一个后台任务，避免滚动/重绘重复触发。
    static MERMAID_RENDER_INFLIGHT: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// `sha256(缓存格式版本 + 源码 + 主题模式)`。主题模式必须编进去，否则切换亮暗模式
/// 不会触发重渲；格式版本必须编进去，否则升级渲染逻辑后旧缓存会一直被复用。
fn source_digest(source: &str, is_dark: bool) -> String {
    let mut hasher = Sha256::new();
    hasher.update(CACHE_FORMAT_VERSION.as_bytes());
    hasher.update(source.as_bytes());
    hasher.update(if is_dark {
        "\0dark".as_bytes()
    } else {
        "\0light".as_bytes()
    });
    format!("{:x}", hasher.finalize())
}

/// SVG 根元素的 `viewBox="x y w h"`（兼容单双引号及多余空白）里取后两个数当自然像素尺寸。
fn parse_viewbox_size(svg: &str) -> Option<(f32, f32)> {
    let idx = svg.find("viewBox")?;
    let rest = &svg[idx + "viewBox".len()..];
    let quote_start = rest.find(['"', '\''])?;
    let quote_char = rest.as_bytes()[quote_start];
    let content = &rest[quote_start + 1..];
    let quote_end = content.find(quote_char as char)?;
    let mut parts = content[..quote_end].split_whitespace();
    let _x = parts.next()?;
    let _y = parts.next()?;
    let w: f32 = parts.next()?.parse().ok()?;
    let h: f32 = parts.next()?.parse().ok()?;
    (w > 0.0 && h > 0.0).then_some((w, h))
}

/// mindmap 按 `17 - 3*i` 给深层 section 写 `stroke-width`，到 `edge-depth-5` 就已经
/// 是 `-1`。浏览器会丢掉非法声明，回落到 `.edge{stroke-width:3}`；resvg / usvg
/// 见到负数会把这条线整段丢掉。这里把负的声明删掉，正的和 0 保持不动。
///
/// 只改标签里的属性和 `<style>` 里的 CSS。文字节点是图里写的字，原样保留：标签里
/// 写着 `stroke-width:-1` 也照样显示。
fn strip_negative_stroke_width(svg: &str) -> String {
    let mut out = String::with_capacity(svg.len());
    let mut rest = svg;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let from_tag = &rest[open..];
        let Some(close) = from_tag.find('>') else {
            out.push_str(from_tag);
            return out;
        };
        let tag = &from_tag[..=close];
        out.push_str(&strip_negative_stroke_width_in(tag));
        rest = &from_tag[close + 1..];
        // `<style>` 的正文是 CSS（可能包在 CDATA 里），一直处理到 `</style>`。
        if tag.starts_with("<style") && !tag.ends_with("/>") {
            let end = rest.find("</style>").unwrap_or(rest.len());
            out.push_str(&strip_negative_stroke_width_in(&rest[..end]));
            rest = &rest[end..];
        }
    }
    out.push_str(rest);
    out
}

/// 在一段属性或 CSS 里删掉负的 `stroke-width` 声明。
fn strip_negative_stroke_width_in(svg: &str) -> String {
    const KEY: &str = "stroke-width";
    let mut out = String::with_capacity(svg.len());
    let mut rest = svg;
    while let Some(idx) = rest.find(KEY) {
        out.push_str(&rest[..idx]);
        let after_key = &rest[idx + KEY.len()..];
        if let Some(kept_from) = negative_stroke_width_tail(after_key) {
            rest = kept_from;
            continue;
        }
        out.push_str(KEY);
        rest = after_key;
    }
    out.push_str(rest);
    out
}

/// `after_key` 是 `stroke-width` 后面的文本。值是负数时返回声明结束后的剩余切片，
/// 调用方把整个声明丢掉；不是负数则返回 `None`，原样保留。
fn negative_stroke_width_tail(after_key: &str) -> Option<&str> {
    let trimmed = after_key.trim_start_matches([' ', '\t', '\n', '\r']);
    let sep = *trimmed.as_bytes().first()?;
    if sep != b':' && sep != b'=' {
        return None;
    }
    let after_sep = &trimmed[1..];
    let (value, quoted) = if sep == b'=' {
        match after_sep.as_bytes().first().copied() {
            Some(q @ (b'"' | b'\'')) => (&after_sep[1..], Some(q)),
            _ => (after_sep, None),
        }
    } else {
        (after_sep, None)
    };
    let value = value.trim_start_matches([' ', '\t', '\n', '\r']);
    let number_len = negative_number_len(value)?;
    let mut end = number_len;
    if sep == b':' && value[end..].starts_with("px") {
        end += 2;
    }
    if let Some(q) = quoted {
        let tail = value[end..].trim_start_matches([' ', '\t', '\n', '\r']);
        let ws = value[end..].len() - tail.len();
        if tail.as_bytes().first().copied() != Some(q) {
            return None;
        }
        end += ws + 1;
    }
    let mut tail_from = end;
    let bytes = value.as_bytes();
    while tail_from < bytes.len() && matches!(bytes[tail_from], b' ' | b'\t' | b'\n' | b'\r') {
        tail_from += 1;
    }
    if sep == b':' && tail_from < bytes.len() && bytes[tail_from] == b';' {
        tail_from += 1;
    }
    Some(&value[tail_from..])
}

fn negative_number_len(value: &str) -> Option<usize> {
    let bytes = value.as_bytes();
    if bytes.first().copied() != Some(b'-') {
        return None;
    }
    let mut i = 1;
    let mut digits = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
        digits += 1;
    }
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
            digits += 1;
        }
    }
    (digits > 0).then_some(i)
}

/// 系统字体库里的一个面。字节是 fontdb 共享出来的文件映射：同一个文件（TTC 里的
/// regular / bold）共用一份，不拷进堆里。
struct LoadedFace {
    family: String,
    data: Arc<dyn AsRef<[u8]> + Send + Sync>,
    index: u32,
    bold: bool,
}

impl LoadedFace {
    fn bytes(&self) -> &[u8] {
        AsRef::<[u8]>::as_ref(&*self.data)
    }
}

fn measure_faces() -> &'static [LoadedFace] {
    static FACES: OnceLock<Vec<LoadedFace>> = OnceLock::new();
    FACES.get_or_init(load_measure_faces)
}

/// 和 `measure_faces` 一一对应的解析结果，解析不了的是 `None`。量每个标签都要用，
/// 只解析一次。
fn parsed_faces() -> &'static [Option<rustybuzz::Face<'static>>] {
    static PARSED: OnceLock<Vec<Option<rustybuzz::Face<'static>>>> = OnceLock::new();
    PARSED.get_or_init(|| {
        measure_faces()
            .iter()
            .map(|face| rustybuzz::Face::from_slice(face.bytes(), face.index))
            .collect()
    })
}

/// GPUI 的 `SvgRenderer` 用 `usvg::fontdb::Database::load_system_fonts()` 扫系统字体
/// （另外还会塞 IBM Plex / Lilex，只用来兜泛型家族）。gpui 不把这份 fontdb 公开出来，
/// 所以这里直接依赖同一个 usvg 0.46，按 SVG 里那条字体栈查询，再退到 sans-serif、
/// 任意一款，以及 Linux 上常见的 CJK 字体。
fn load_measure_faces() -> Vec<LoadedFace> {
    let mut db = usvg::fontdb::Database::new();
    db.load_system_fonts();
    let mut faces = Vec::new();
    let mut seen = HashSet::new();

    for name in font_family_names(MERMAID_FONT_FAMILY) {
        if is_generic_family(&name) {
            continue;
        }
        push_named_family(&mut db, &name, &mut faces, &mut seen);
    }
    let regular = query_sans(&db, 400);
    let bold = query_sans(&db, 700);
    push_queried_family(&mut db, "sans-serif", regular, bold, &mut faces, &mut seen);
    if !faces_cover(&faces, '中') {
        for name in CJK_MEASURE_FALLBACKS {
            push_named_family(&mut db, name, &mut faces, &mut seen);
            if faces_cover(&faces, '中') {
                break;
            }
        }
    }
    if faces.is_empty() {
        let fallback = db
            .faces()
            .next()
            .map(|info| (info.id, info.weight.0 >= 600));
        if let Some((id, bold)) = fallback {
            push_face(&mut db, id, "fallback", bold, &mut faces, &mut seen);
        }
    }
    faces
}

fn push_named_family(
    db: &mut usvg::fontdb::Database,
    family: &str,
    faces: &mut Vec<LoadedFace>,
    seen: &mut HashSet<usvg::fontdb::ID>,
) {
    let regular = query_named(db, family, 400);
    let bold = query_named(db, family, 700);
    push_queried_family(db, family, regular, bold, faces, seen);
}

fn push_queried_family(
    db: &mut usvg::fontdb::Database,
    family: &str,
    regular: Option<usvg::fontdb::ID>,
    bold: Option<usvg::fontdb::ID>,
    faces: &mut Vec<LoadedFace>,
    seen: &mut HashSet<usvg::fontdb::ID>,
) {
    let weight = |db: &usvg::fontdb::Database, id| db.face(id).map_or(400, |face| face.weight.0);
    if let Some(id) = regular {
        let is_bold = weight(&*db, id) >= 600;
        push_face(db, id, family, is_bold, faces, seen);
    }
    if let Some(id) = bold
        && weight(&*db, id) >= 600
    {
        push_face(db, id, family, true, faces, seen);
    }
}

/// 按 fontdb 的面 id 去重。不能按数据指针：`load_system_fonts` 的面是按需映射的，
/// 用完就解除，下一个文件很可能映射到同一个地址。
fn push_face(
    db: &mut usvg::fontdb::Database,
    id: usvg::fontdb::ID,
    family: &str,
    bold: bool,
    faces: &mut Vec<LoadedFace>,
    seen: &mut HashSet<usvg::fontdb::ID>,
) {
    if !seen.insert(id) {
        return;
    }
    // SAFETY: 映射的是系统字体文件，进程里一直留着。fontdb 的说明是别的进程原地改写
    // 这个文件时，映射里可能看到改动甚至崩溃；系统字体只会整个替换（新 inode），旧映射
    // 不受影响。GPUI 栅格化 SVG 时也是这样映射同一批文件。
    let Some((data, index)) = (unsafe { db.make_shared_face_data(id) }) else {
        return;
    };
    faces.push(LoadedFace {
        family: family.to_string(),
        data,
        index,
        bold,
    });
}

fn query_named(db: &usvg::fontdb::Database, family: &str, weight: u16) -> Option<usvg::fontdb::ID> {
    let name = usvg::fontdb::Family::Name(family);
    db.query(&font_query(&[name], weight))
}

fn query_sans(db: &usvg::fontdb::Database, weight: u16) -> Option<usvg::fontdb::ID> {
    db.query(&font_query(&[usvg::fontdb::Family::SansSerif], weight))
}

fn font_query<'a>(
    families: &'a [usvg::fontdb::Family<'a>],
    weight: u16,
) -> usvg::fontdb::Query<'a> {
    usvg::fontdb::Query {
        families,
        weight: usvg::fontdb::Weight(weight),
        stretch: usvg::fontdb::Stretch::Normal,
        style: usvg::fontdb::Style::Normal,
    }
}

fn faces_cover(faces: &[LoadedFace], ch: char) -> bool {
    faces.iter().any(|face| face_covers(face, ch))
}

fn face_covers(face: &LoadedFace, ch: char) -> bool {
    rustybuzz::Face::from_slice(face.bytes(), face.index)
        .and_then(|parsed| parsed.glyph_index(ch))
        .is_some()
}

fn is_generic_family(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "sans-serif"
            | "serif"
            | "monospace"
            | "cursive"
            | "fantasy"
            | "system-ui"
            | "ui-sans-serif"
            | "emoji"
    )
}

/// 拆 CSS `font-family` 列表，去掉引号。引号里的逗号不当分隔符。
fn font_family_names(stack: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for ch in stack.chars() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => current.push(ch),
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch == ',' => {
                push_family_name(&mut names, &mut current);
            }
            None => current.push(ch),
        }
    }
    push_family_name(&mut names, &mut current);
    names
}

fn push_family_name(names: &mut Vec<String>, current: &mut String) {
    let name = current.trim();
    if !name.is_empty() {
        names.push(name.to_string());
    }
    current.clear();
}

fn text_measurement_policy() -> &'static TextMeasurementPolicy {
    static POLICY: OnceLock<TextMeasurementPolicy> = OnceLock::new();
    POLICY.get_or_init(|| {
        let _ = measure_faces();
        let measurer =
            DeterministicTextMeasurer::default().with_width_callback(measure_label_width);
        let profile =
            MeasurementProfileId::new("smelt.system-fontdb").expect("测宽 profile 名字非空");
        let identity =
            TextMeasurementProfileIdentity::new(profile, "1").expect("测宽 profile 版本非空");
        TextMeasurementPolicy::uniform(TextMeasurementProfile::new(identity, measurer))
    })
}

/// merman 的宽度回调。U+00AD 必须原样交给 shaper，不能先删掉。
/// 回调得返回有限、非负的 CSS 像素。
fn measure_label_width(text: &str, style: &TextStyle) -> f64 {
    if text.is_empty() {
        return 0.0;
    }
    let font_size = if style.font_size.is_finite() && style.font_size > 0.0 {
        style.font_size
    } else {
        return 0.0;
    };
    let faces = measure_faces();
    if faces.is_empty() {
        return heuristic_width(text, font_size, style_is_bold(style));
    }
    let parsed = parsed_faces();
    let preferred = style
        .font_family
        .as_deref()
        .map(font_family_names)
        .filter(|names| !names.is_empty())
        .unwrap_or_else(|| font_family_names(MERMAID_FONT_FAMILY));
    let bold = style_is_bold(style);

    let mut total = 0.0;
    let mut run = String::new();
    let mut run_face: Option<usize> = None;
    let mut started = false;
    for ch in text.chars() {
        let idx = pick_face(parsed, faces, ch, &preferred, bold);
        if started && run_face != idx {
            total += run_width(parsed, faces, run_face, &run, font_size, bold);
            run.clear();
        }
        started = true;
        run_face = idx;
        run.push(ch);
    }
    total += run_width(parsed, faces, run_face, &run, font_size, bold);
    if total.is_finite() && total >= 0.0 {
        total
    } else {
        0.0
    }
}

fn pick_face(
    parsed: &[Option<rustybuzz::Face<'_>>],
    faces: &[LoadedFace],
    ch: char,
    preferred: &[String],
    bold: bool,
) -> Option<usize> {
    let mut best: Option<(u16, usize)> = None;
    for (index, face) in parsed.iter().enumerate() {
        let Some(face) = face else {
            continue;
        };
        if face.glyph_index(ch).is_none() {
            continue;
        }
        let family_rank = preferred
            .iter()
            .position(|name| name.eq_ignore_ascii_case(&faces[index].family))
            .map(|pos| pos as u16)
            .unwrap_or(200);
        let bold_penalty = u16::from(bold != faces[index].bold);
        let rank = family_rank.saturating_mul(2).saturating_add(bold_penalty);
        if best.is_none_or(|(prev, _)| rank < prev) {
            best = Some((rank, index));
        }
    }
    best.map(|(_, index)| index)
}

fn run_width(
    parsed: &[Option<rustybuzz::Face<'_>>],
    faces: &[LoadedFace],
    index: Option<usize>,
    text: &str,
    font_size: f64,
    bold: bool,
) -> f64 {
    let Some(index) = index else {
        return heuristic_width(text, font_size, bold);
    };
    let Some(face) = parsed.get(index).and_then(Option::as_ref) else {
        return heuristic_width(text, font_size, bold);
    };
    let width = shape_width(face, text, font_size);
    if bold && !faces[index].bold {
        width * SYNTHETIC_BOLD_SCALE
    } else {
        width
    }
}

fn shape_width(face: &rustybuzz::Face<'_>, text: &str, font_size: f64) -> f64 {
    let upem = f64::from(face.units_per_em());
    if upem <= 0.0 {
        return heuristic_width(text, font_size, false);
    }
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    let glyphs = rustybuzz::shape(face, &[], buffer);
    let advance: i64 = glyphs
        .glyph_positions()
        .iter()
        .map(|pos| i64::from(pos.x_advance))
        .sum();
    let width = advance as f64 / upem * font_size;
    if width.is_finite() && width >= 0.0 {
        width
    } else {
        0.0
    }
}

fn style_is_bold(style: &TextStyle) -> bool {
    style.font_weight.as_deref().is_some_and(|weight| {
        weight.eq_ignore_ascii_case("bold")
            || weight.eq_ignore_ascii_case("bolder")
            || weight.parse::<u32>().is_ok_and(|value| value >= 600)
    })
}

/// 没有任何面能画出这个字符时的宽度。CJK 按 1em、ASCII 按 0.55em，略偏宽，
/// 避免类图 `overflow:hidden` 把字裁掉。
fn heuristic_width(text: &str, font_size: f64, bold: bool) -> f64 {
    let em: f64 = text.chars().map(heuristic_em).sum();
    let scale = if bold { SYNTHETIC_BOLD_SCALE } else { 1.0 };
    em * font_size * scale
}

fn heuristic_em(ch: char) -> f64 {
    if ch == '\u{00AD}' || ch.is_control() {
        return 0.0;
    }
    match u32::from(ch) {
        0x1100..=0x115F
        | 0x2329..=0x232A
        | 0x2E80..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE10..=0xFE19
        | 0xFE30..=0xFE6F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1F64F
        | 0x20000..=0x3FFFD => 1.0,
        0x20..=0x7E => 0.55,
        _ => 0.7,
    }
}

fn renderer(is_dark: bool) -> &'static Renderer {
    fn build(is_dark: bool) -> Renderer {
        let config = MermaidConfig::from_value(serde_json::json!({
            "theme": if is_dark { "dark" } else { "default" },
            "fontFamily": MERMAID_FONT_FAMILY,
        }));
        Renderer::new().with_engine(Engine::new().with_site_config(config))
    }
    static LIGHT: OnceLock<Renderer> = OnceLock::new();
    static DARK: OnceLock<Renderer> = OnceLock::new();
    if is_dark {
        DARK.get_or_init(|| build(true))
    } else {
        LIGHT.get_or_init(|| build(false))
    }
}

fn svg_request() -> SvgRequest {
    SvgRequest {
        pipeline: Some(SvgPipeline::resvg_safe()),
        environment: SvgEnvironment::deterministic()
            .with_text_measurement_policy(text_measurement_policy().clone()),
        ..Default::default()
    }
}

fn render_svg(source: &str, is_dark: bool) -> Result<String, String> {
    match std::panic::catch_unwind(|| render_svg_inner(source, is_dark)) {
        Ok(result) => result,
        Err(_) => Err("渲染中断".into()),
    }
}

fn render_svg_inner(source: &str, is_dark: bool) -> Result<String, String> {
    let output = renderer(is_dark)
        .render(RenderRequest::svg(
            source,
            OperationControl::new(),
            svg_request(),
        ))
        .map_err(|err| err.to_string())?;
    match output {
        RenderOutput::Svg(Some(svg)) => Ok(strip_negative_stroke_width(svg.svg())),
        RenderOutput::Svg(None) => Err("源码里没有 mermaid 图".into()),
        _ => Err("渲染结果不是 SVG".into()),
    }
}

fn render_or_load(source: &str, is_dark: bool) -> Result<MermaidImage, String> {
    let svg = render_svg(source, is_dark)?;
    let (width, height) = parse_viewbox_size(&svg).unwrap_or((400.0, 300.0));
    let image = gpui::Image::from_bytes(gpui::ImageFormat::Svg, svg.into_bytes());
    Ok(MermaidImage {
        image: Arc::new(image),
        width,
        height,
    })
}

/// renderer 阶段：查内存缓存 → 未命中调 `render_or_load` 并写回 → 渲成 `img()`；
/// 渲染失败给一个提示条 + 原始 mermaid 源码兜底，不留白、不 panic。
fn render_mermaid_block(node: &MarkdownNode, _window: &mut Window, cx: &mut App) -> AnyElement {
    let Some(data) = node.data::<MermaidBlockData>() else {
        return div().child(node.as_text().to_string()).into_any_element();
    };

    let is_dark = cx.theme().is_dark();
    let digest = source_digest(&data.source, is_dark);

    if let Some(result) = MERMAID_RENDER_CACHE.with(|c| c.borrow().get(&digest).cloned()) {
        return mermaid_result_element(result, &digest, &data.source, cx);
    }

    // 缓存未命中但已有在途任务 → 占位，等后台完成刷新后重渲染命中缓存。
    if MERMAID_RENDER_INFLIGHT.with(|c| c.borrow().contains(&digest)) {
        return div()
            .id(SharedString::from(format!("mermaid-{digest}-pending")))
            .into_any_element();
    }

    // 首次见：渲染挪后台线程，完成后只写内存缓存，下一帧直接命中。
    MERMAID_RENDER_INFLIGHT.with(|c| c.borrow_mut().insert(digest.clone()));
    let source = data.source.clone();
    let digest_for_task = digest.clone();
    cx.spawn(async move |cx| {
        let outcome = cx
            .background_executor()
            .spawn(async move { render_or_load(&source, is_dark) })
            .await
            .map(|img| MermaidRender::Ok(Arc::new(img)))
            .unwrap_or_else(|e| MermaidRender::Err(e.into()));
        MERMAID_RENDER_INFLIGHT.with(|c| c.borrow_mut().remove(&digest_for_task));
        MERMAID_RENDER_CACHE.with(|c| c.borrow_mut().insert(digest_for_task, outcome));
        cx.update(|cx| cx.refresh_windows());
    })
    .detach();

    div()
        .id(SharedString::from(format!("mermaid-{digest}-pending")))
        .into_any_element()
}

fn mermaid_result_element(
    result: MermaidRender,
    digest: &str,
    source: &str,
    cx: &App,
) -> AnyElement {
    match result {
        MermaidRender::Ok(m) => {
            let preview = m.clone();
            // 外层 flex 行撑满列宽；触发区按图的实际宽度收缩，空白处不响应点击。
            // 点开预览不是链接，光标保持默认箭头。
            div()
                .id(SharedString::from(format!("mermaid-{digest}")))
                .flex()
                .child(
                    div()
                        .id("preview-trigger")
                        .debug_selector(|| "MERMAID_PREVIEW_TRIGGER".to_string())
                        .flex()
                        .on_click(move |_, window, cx| {
                            open_mermaid_preview(preview.clone(), window, cx);
                        })
                        .child(MermaidInline { image: m }),
                )
                .into_any_element()
        }
        MermaidRender::Err(msg) => div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_color(cx.theme().warning_foreground)
                    .child(format!("⚠ mermaid 渲染失败：{msg}")),
            )
            .child(
                div()
                    .font_family(smelt_model::font_config::font_family())
                    .text_color(cx.theme().foreground)
                    .child(source.to_string()),
            )
            .into_any_element(),
    }
}

/// 全仓库统一的 markdown 渲染入口，替代直接调 `TextView::markdown`——多接的这两
/// 个钩子就是本文件的 mermaid 支持，以后要加别的自定义 block 渲染也从这里改，
/// 不要在调用点各自散开接。
///
/// `TextView::markdown` 默认 `selectable(false)`，各调用点需要正文可复制时统一打开。
pub fn markdown_view(id: impl Into<ElementId>, text: impl Into<SharedString>) -> TextView {
    markdown_view_with_selection(id, text, true)
}

/// ACP 对话里的 Markdown：正文可框选复制，同时链接保持可点击——gpui-component
/// 的链接打开（MouseUp）会先检查是否已有文本选择，拖拽选择时不会误触链接，
/// 因此打开 selectable 不会破坏链接点击。
pub fn markdown_view_clickable(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
) -> TextView {
    markdown_view_with_selection(id, text, true)
}

fn markdown_text_view_style() -> TextViewStyle {
    // 围栏代码块走 `cx.theme().highlight_theme` / `tokens.muted`（= bg_card）。
    // 行内 code 面积小，同样用卡片底会糊进正文，改用选中表面拉开对比。
    let inline_code = HighlightStyle {
        background_color: Some(rgb(crate::ui_theme::bg_selected()).into()),
        ..Default::default()
    };
    // 右上角挂了语言名和复制按钮（见 `code_block_corner`），首行要让开它，跟
    // gpui-component 自带语言标签时的留白一致。
    let code_block = gpui::StyleRefinement::default().pt(px(28.));
    let mut style = TextViewStyle::default()
        .inline_code(inline_code)
        .code_block(code_block);
    style.is_dark = !crate::ui_theme::is_light();
    style
}

/// 围栏代码块右上角：语言名 + 复制按钮。
///
/// 一旦接了 `code_block_actions`，gpui-component 就不再画它自带的语言标签，所以
/// 语言名在这里一并画。按钮 id 用固定的 `"copy"` 即可：框架按代码块给 actions
/// 套了一层带位置的 id，不同代码块之间不会撞。
fn code_block_corner(
    code_block: &gpui_base::text::CodeBlock,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let lang = code_block.lang().filter(|lang| !lang.is_empty());
    gpui_component::h_flex()
        .gap_1()
        .items_center()
        .children(lang.map(|lang| {
            div()
                .px_1()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(lang)
        }))
        .child(
            gpui_component::clipboard::Clipboard::new("copy")
                .value(code_block.code())
                .icon_action("复制代码"),
        )
        .into_any_element()
}

fn markdown_view_with_selection(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
    selectable: bool,
) -> TextView {
    with_markdown_hooks(
        TextView::markdown(id, text).selectable(selectable),
        shared_markdown_extensions(),
    )
}

/// 对话消息（ACP 对话、会话历史）的 Markdown，解析状态由调用方持有。原始 HTML 按
/// 原文显示，见 `markdown_literal_html` 模块；文件链接点击时按 `links` 解析，见
/// [`crate::markdown_links`]。
///
/// `TextView::markdown` 把解析结果挂在元素上，元素有一帧没画就丢掉。虚拟列表里的行
/// 滚出渲染范围再回来，超过 4KB 的正文要先以空内容等后台重新解析，行高跟着跳。
/// 这类地方自己留住 `TextViewState`，用这个入口画。
pub fn chat_markdown_view(
    state: &gpui::Entity<TextViewState>,
    links: crate::markdown_links::FileLinks<'_>,
) -> TextView {
    with_markdown_hooks(
        TextView::new(state).selectable(true),
        chat_markdown_extensions(),
    )
    .on_link_click(crate::markdown_links::link_opener(links))
}

fn with_markdown_hooks(view: TextView, extensions: &MarkdownExtensions) -> TextView {
    view.style(markdown_text_view_style())
        .code_block_actions(code_block_corner)
        .markdown_extensions(extensions.clone())
}

/// `MarkdownExtensions` assigns a new global revision whenever a parser or renderer is registered.
/// Rebuilding it from the immediate-mode render path therefore makes every existing `TextViewState`
/// synchronously reparse unchanged Markdown on every frame. Keep one immutable registry so cloned
/// views retain a stable revision.
fn shared_markdown_extensions() -> &'static MarkdownExtensions {
    static EXTENSIONS: OnceLock<MarkdownExtensions> = OnceLock::new();
    EXTENSIONS.get_or_init(|| {
        MarkdownExtensions::default()
            .block_parser(parse_mermaid_block)
            .block_renderer("mermaid", render_mermaid_block)
    })
}

/// [`shared_markdown_extensions`] 再加上原始 HTML 按原文显示。同样只建一次。
fn chat_markdown_extensions() -> &'static MarkdownExtensions {
    static EXTENSIONS: OnceLock<MarkdownExtensions> = OnceLock::new();
    EXTENSIONS.get_or_init(|| {
        shared_markdown_extensions()
            .clone()
            .plugin(crate::markdown_literal_html::LiteralInlineHtml)
            .plugin(crate::markdown_literal_html::LiteralBlockHtml)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ChatMarkdownRoot {
        state: gpui::Entity<TextViewState>,
    }

    impl gpui::Render for ChatMarkdownRoot {
        fn render(
            &mut self,
            _window: &mut Window,
            _cx: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            div().w(px(600.)).child(chat_markdown_view(
                &self.state,
                crate::markdown_links::FileLinks::default(),
            ))
        }
    }

    /// 对话里的原始 HTML 原样显示：不认识的标签不吞内容，代码里不多出 `\`。
    #[gpui::test]
    fn chat_markdown_shows_raw_html_as_written(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let source = "前 <section class=\"card\">内容</section> 和 <panel>\n\n\
                      <div>\n块里的字\n</div>\n\n\
                      1. 步骤\n   - 子项\n\n     ```rust\n     let v: Vec<String> = vec![];\n     ```\n";
        let (root, cx) = cx.add_window_view(|_window, cx| ChatMarkdownRoot {
            state: cx.new(|cx| TextViewState::markdown(source, cx)),
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.run_until_parked();

        let text = cx.update(|_window, cx| {
            let state = root.read(cx).state.clone();
            state.update(cx, |state, cx| {
                state.select_all(cx);
                state.selected_text()
            })
        });
        assert!(
            text.contains("<section class=\"card\">内容</section>"),
            "{text}"
        );
        assert!(text.contains("<panel>"), "{text}");
        assert!(text.contains("<div>\n块里的字\n</div>"), "{text}");
        assert!(text.contains("let v: Vec<String> = vec![];"), "{text}");
        assert!(!text.contains('\\'), "{text}");
    }

    #[test]
    fn markdown_extensions_are_reused_across_views() {
        assert!(std::ptr::eq(
            shared_markdown_extensions(),
            shared_markdown_extensions()
        ));
    }

    #[gpui::test]
    fn clicking_mermaid_opens_the_zoom_preview(cx: &mut gpui::TestAppContext) {
        use gpui::VisualTestContext;

        cx.update(gpui_component::init);
        let source = "```mermaid\nflowchart TD\nA-->B\n```";
        let (_root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| ChatMarkdownRoot {
                state: cx.new(|cx| TextViewState::markdown(source, cx)),
            });
            gpui_component::Root::new(view, window, cx)
        });
        let cx: &mut VisualTestContext = cx;
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let trigger = cx
            .debug_bounds("MERMAID_PREVIEW_TRIGGER")
            .expect("Mermaid 图表应可点击打开预览");
        let natural = render_or_load("flowchart TD\nA-->B", false).expect("应能渲染");
        assert!(
            (f32::from(trigger.size.height) - natural.height).abs() < 1.0,
            "内嵌图按 SVG 自然高度显示"
        );
        cx.simulate_click(trigger.center(), Default::default());
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            cx.debug_bounds("dialog-layer").is_some(),
            "点击图表后应打开带缩放控件的预览对话框"
        );
        assert!(cx.debug_bounds("MERMAID_PREVIEW_ZOOM_OUT").is_some());
        assert!(cx.debug_bounds("MERMAID_PREVIEW_ZOOM_IN").is_some());
        assert!(cx.debug_bounds("MERMAID_PREVIEW_ZOOM_FIT").is_some());
    }

    #[test]
    fn inline_mermaid_fits_the_column_without_upscaling() {
        // 比列宽的图等比缩到列宽，高度跟着缩，不留白。
        assert_eq!(
            mermaid_inline_size(1800.0, 900.0, Some(600.0)),
            size(px(600.), px(300.))
        );
        // 比列窄的图保持自然尺寸，不放大。
        assert_eq!(
            mermaid_inline_size(300.0, 200.0, Some(600.0)),
            size(px(300.), px(200.))
        );
        // 没有宽度约束（max-content）时报自然尺寸，最小内容宽度为 0。
        assert_eq!(
            mermaid_inline_size(300.0, 200.0, None),
            size(px(300.), px(200.))
        );
        assert_eq!(
            mermaid_inline_size(300.0, 200.0, Some(0.0)),
            size(px(0.), px(0.))
        );
    }

    #[test]
    fn preview_zoom_steps_stay_within_bounds() {
        assert_eq!(preview_zoom_in(MAX_PREVIEW_SCALE), MAX_PREVIEW_SCALE);
        assert_eq!(preview_zoom_out(MIN_PREVIEW_SCALE), MIN_PREVIEW_SCALE);
        assert!((preview_zoom_in(1.0) - 1.25).abs() < f32::EPSILON);
        assert!((preview_zoom_out(1.0) - 0.8).abs() < 0.0001);
    }

    #[test]
    fn preview_fit_uses_the_tighter_axis() {
        let viewport = size(px(600.), px(400.));
        assert!((preview_fit_scale(1200.0, 600.0, viewport) - 0.5).abs() < f32::EPSILON);
        assert!((preview_fit_scale(100.0, 200.0, viewport) - 2.0).abs() < f32::EPSILON);
    }

    #[test]
    fn preview_zoom_keeps_the_viewport_center_on_the_same_point() {
        // 1000 宽的图在 400 视口里滚到 -300，中心对着图上 500 处（正中）。放大到 2000
        // 后中心仍对着正中：偏移 = 200 - 1000 = -800。
        assert_eq!(
            preview_anchored_offset(-300.0, 400.0, 1000.0, 2000.0),
            -800.0
        );
        // 图原本比视口窄（居中摆放），放大后从图中点开始。
        assert_eq!(preview_anchored_offset(0.0, 400.0, 300.0, 600.0), -100.0);
        // 缩到比视口还小时偏移归零，不会滚出内容。
        assert_eq!(preview_anchored_offset(-800.0, 400.0, 2000.0, 300.0), 0.0);
        // 结果夹在可滚动范围内。
        assert_eq!(
            preview_anchored_offset(-600.0, 400.0, 1000.0, 1000.0),
            -600.0
        );
        assert_eq!(
            preview_anchored_offset(-600.0, 400.0, 1000.0, 500.0),
            -100.0
        );
    }

    #[test]
    fn preview_raster_follows_the_display_scale_and_stays_capped() {
        // 2 倍屏上放大到 3 倍：每个自然像素对应 6 个设备像素。
        assert_eq!(
            preview_raster_size(400.0, 300.0, 6.0),
            size(DevicePixels(2400), DevicePixels(1800))
        );
        // 大图放到最大倍数：长边、总像素都不超过上限，并保持比例。
        let capped = preview_raster_size(3000.0, 1500.0, MAX_PREVIEW_SCALE * 2.0);
        assert!(capped.width.0 as f32 <= MAX_RASTER_SIDE);
        assert!(capped.width.0 as f32 * capped.height.0 as f32 <= MAX_RASTER_PIXELS * 1.001);
        assert!((capped.width.0 as f32 / capped.height.0 as f32 - 2.0).abs() < 0.01);
    }

    #[test]
    fn small_viewport_jitter_does_not_rerasterize() {
        let base = size(DevicePixels(1000), DevicePixels(800));
        assert!(raster_close_enough(
            size(DevicePixels(1010), DevicePixels(795)),
            base
        ));
        assert!(!raster_close_enough(
            size(DevicePixels(1250), DevicePixels(1000)),
            base
        ));
    }

    #[test]
    fn dragging_preview_moves_the_scroll_origin_with_the_pointer() {
        let offset = preview_pan_offset(
            point(px(-20.), px(-60.)),
            point(px(100.), px(100.)),
            point(px(140.), px(80.)),
        );
        assert_eq!(offset, point(px(20.), px(-80.)));
    }

    #[test]
    fn valid_source_renders_without_writing_disk() {
        let img = render_or_load("flowchart TD\nA-->B", false).expect("应能渲染");
        assert!(img.width > 0.0 && img.height > 0.0);
        let svg = render_svg("flowchart TD\nA-->B", false).expect("应能渲出 SVG");
        assert!(svg.trim_start().starts_with("<svg"));
        assert!(svg.contains("width=\"100%\""));
    }

    /// 字体栈来自站点配置，不再做 rusty-mermaid 那次字符串替换。GPUI 的 usvg
    /// 要在 `font-family` 里看到能对上的字面名。
    #[test]
    fn rendered_svg_uses_cjk_font_stack() {
        let img = render_or_load("flowchart TD\nA[开始]-->B[结束]", false).expect("应能渲染");
        let svg = render_svg("flowchart TD\nA[开始]-->B[结束]", false).unwrap();
        assert!(
            svg.contains("Hiragino Sans GB"),
            "渲染产物应该带上配置的 CJK 字体栈"
        );
        assert!(img.width > 0.0 && img.height > 0.0);
    }

    #[test]
    fn invalid_source_errs_without_panicking() {
        assert!(render_or_load("this is not mermaid at all {{{", false).is_err());
    }

    #[test]
    fn theme_mode_changes_cache_key_and_svg() {
        let light = source_digest("graph TD\nA-->B", false);
        let dark = source_digest("graph TD\nA-->B", true);
        assert_ne!(light, dark);
        let light_svg = render_svg("graph TD\nA-->B", false).expect("亮色应能渲染");
        let dark_svg = render_svg("graph TD\nA-->B", true).expect("暗色应能渲染");
        assert_ne!(light_svg, dark_svg);
    }

    #[test]
    fn parser_only_matches_mermaid_lang() {
        use markdown_ast::{Code, Node};

        let mermaid_node = Node::Code(Code {
            lang: Some("mermaid".to_string()),
            meta: None,
            value: "graph TD\nA-->B".to_string(),
            position: None,
        });
        let rust_node = Node::Code(Code {
            lang: Some("rust".to_string()),
            meta: None,
            value: "fn main() {}".to_string(),
            position: None,
        });

        assert_eq!(
            mermaid_source(&mermaid_node).as_deref(),
            Some("graph TD\nA-->B")
        );
        assert!(mermaid_source(&rust_node).is_none());
    }

    #[test]
    fn parse_viewbox_size_handles_single_quotes_and_whitespace() {
        assert_eq!(
            super::parse_viewbox_size("<svg viewBox='0 0 500 300'></svg>"),
            Some((500.0, 300.0))
        );
        assert_eq!(
            super::parse_viewbox_size("<svg viewBox=\" 0  0   800.5  600.25 \"></svg>"),
            Some((800.5, 600.25))
        );
        assert_eq!(
            super::parse_viewbox_size("<svg viewBox=\"0 0 -10 20\"></svg>"),
            None
        );
    }

    #[test]
    fn strip_negative_stroke_width_keeps_positive_rules() {
        let raw = "<svg><style>#mm .edge-depth-5{stroke-width:-1;}#mm .edge{stroke-width:3;}</style><line stroke-width=\"-4\" stroke=\"red\"/></svg>";
        let fixed = super::strip_negative_stroke_width(raw);
        assert_eq!(
            fixed,
            "<svg><style>#mm .edge-depth-5{}#mm .edge{stroke-width:3;}</style><line stroke=\"red\"/></svg>"
        );
    }

    /// 图里写的字原样显示，哪怕正好是一句负的 `stroke-width`。
    #[test]
    fn strip_negative_stroke_width_leaves_text_alone() {
        let raw = "<svg><style><![CDATA[.a > .b{stroke-width:-2}]]></style><text x=\"1\" style=\"stroke-width:-3\">stroke-width:-1</text></svg>";
        let fixed = super::strip_negative_stroke_width(raw);
        assert_eq!(
            fixed,
            "<svg><style><![CDATA[.a > .b{}]]></style><text x=\"1\" style=\"\">stroke-width:-1</text></svg>"
        );
    }

    #[test]
    fn font_family_names_keep_quoted_spaces() {
        let names = super::font_family_names(MERMAID_FONT_FAMILY);
        assert_eq!(
            names,
            vec![
                "Hiragino Sans GB",
                "Heiti SC",
                "PingFang SC",
                "Helvetica Neue",
                "Arial",
                "sans-serif",
            ]
        );
    }

    /// 用 GPUI 同一份 usvg 0.46 解析 merman 的 `resvg_safe` 输出。样本覆盖
    /// rusty-mermaid 已知会坏的几类：中文子图 id、带回边和 classDef 的流程图、
    /// 中文复合状态、sequence 的 alt/loop/note、类图、带注释的 ER、mindmap、gantt。
    #[test]
    fn samples_render_svg_that_gpui_usvg_parses() {
        let samples = [
            (
                "flowchart",
                "flowchart TD\n\
                 subgraph 规划\n\
                 A[开始] -->|下一步| B[处理]\n\
                 end\n\
                 B --> C{判断}\n\
                 C -->|是| A\n\
                 C --> D@{ shape: diam, label: \"菱形\" }\n\
                 classDef done fill:#dfd,stroke:#333\n\
                 class A done\n",
            ),
            (
                "state",
                "stateDiagram-v2\n\
                 [*] --> 空闲\n\
                 state 工作中 {\n\
                 [*] --> 编写\n\
                 编写 --> 审查\n\
                 }\n\
                 空闲 --> 工作中\n\
                 工作中 --> 空闲\n",
            ),
            (
                "sequence",
                "sequenceDiagram\n\
                 participant 用户\n\
                 participant 服务\n\
                 用户->>服务: 请求\n\
                 alt 成功\n\
                 服务-->>用户: 结果\n\
                 else 失败\n\
                 服务-->>用户: 错误\n\
                 end\n\
                 loop 重试\n\
                 用户->>服务: 再试\n\
                 end\n\
                 Note over 用户,服务: 备注\n",
            ),
            (
                "class",
                "classDiagram\n\
                 class 订单 {\n\
                 +String 编号\n\
                 +确认()\n\
                 }\n\
                 class 用户 {\n\
                 +String 姓名\n\
                 }\n\
                 用户 \"1\" --> \"*\" 订单 : 下单\n",
            ),
            (
                "er",
                "erDiagram\n\
                 USER ||--o{ ORDER : places\n\
                 USER {\n\
                 string email\n\
                 string name\n\
                 }\n\
                 %% SHOULD_NOT_MERGE\n\
                 ORDER {\n\
                 int total\n\
                 }\n",
            ),
            (
                "mindmap",
                "mindmap\n  root((主题))\n    一级\n      二级\n        三级\n          四级\n            五级\n              六级\n                七级\n",
            ),
            (
                "gantt",
                "gantt\n\
                 title 排期\n\
                 dateFormat YYYY-MM-DD\n\
                 section 设计\n\
                 方案 :a1, 2024-01-01, 7d\n\
                 section 开发\n\
                 实现 :a2, after a1, 10d\n\
                 验收 :a3, after a2, 3d\n",
            ),
        ];

        for (name, source) in samples {
            for dark in [false, true] {
                let svg = render_svg(source, dark)
                    .unwrap_or_else(|err| panic!("{name} dark={dark} 渲染失败: {err}"));
                assert!(svg.trim_start().starts_with("<svg"), "{name} 应该是 svg 根");
                assert!(svg.contains("width=\"100%\""), "{name} 根宽度应为 100%");
                let (width, height) =
                    parse_viewbox_size(&svg).unwrap_or_else(|| panic!("{name} 应有正的 viewBox"));
                assert!(width > 0.0 && height > 0.0, "{name} viewBox");
                assert!(
                    !svg.contains("stroke-width:-"),
                    "{name} 不应再留下负的 stroke-width"
                );
                assert_gpui_usvg_parses(name, &svg);
            }
        }

        let flow = render_svg(samples[0].1, false).unwrap();
        assert!(flow.contains("规划"), "中文子图 id 应该进 SVG");
        assert!(flow.contains("下一步"));
        assert!(flow.contains("菱形"));
        assert!(flow.contains("Hiragino Sans GB"));

        let state = render_svg(samples[1].1, false).unwrap();
        assert!(state.contains("空闲"));
        assert!(state.contains("工作中"));

        let sequence = render_svg(samples[2].1, false).unwrap();
        assert!(sequence.contains("备注"));
        assert!(sequence.contains("重试"));

        let class = render_svg(samples[3].1, false).unwrap();
        assert!(class.contains("订单"));
        assert!(class.contains("确认"));

        let er = render_svg(samples[4].1, false).unwrap();
        assert!(er.contains("email"));
        assert!(
            !er.contains("SHOULD_NOT_MERGE"),
            "ER 注释不应进属性名或标签"
        );

        let mindmap = render_svg(samples[5].1, false).unwrap();
        assert!(mindmap.contains("主题"));

        let gantt = render_svg(samples[6].1, false).unwrap();
        assert!(gantt.contains("排期") || gantt.contains("方案"));
    }

    /// 不编 `layout-elk`：没写布局的图、写了 `layout: elk` 的图都按 dagre 排，和显式
    /// `layout: dagre` 一样。有人打开这个 feature，这里会先挂，提醒去看许可证和默认布局。
    #[test]
    fn flowcharts_lay_out_with_dagre_even_when_elk_is_requested() {
        let body = "flowchart TD\nA[开始] --> B[处理]\nB --> C[结束]\nC --> A\n";
        let elk_src = format!("---\nconfig:\n  layout: elk\n---\n{body}");
        let dagre_src = format!("---\nconfig:\n  layout: dagre\n---\n{body}");

        let plain = render_svg(body, false).expect("默认布局应能渲染");
        let elk = render_svg(&elk_src, false).expect("layout: elk 应退回 dagre 渲染");
        let dagre = render_svg(&dagre_src, false).expect("layout: dagre 应能渲染");
        assert_gpui_usvg_parses("layout-elk", &elk);

        let dagre_box = parse_viewbox_size(&dagre).expect("dagre 应有 viewBox");
        assert_eq!(
            parse_viewbox_size(&plain),
            Some(dagre_box),
            "默认应是 dagre"
        );
        assert_eq!(
            parse_viewbox_size(&elk),
            Some(dagre_box),
            "elk 请求应退回 dagre"
        );
    }

    /// gpui-pre 0.3.9 栅格化 SVG 用的就是 usvg 0.46。这里用同一份 crate 解析，
    /// 确认 `resvg_safe` 的输出不会被这版 usvg 拒掉。
    fn assert_gpui_usvg_parses(name: &str, svg: &str) {
        let options = usvg::Options::default();
        usvg::Tree::from_str(svg, &options)
            .unwrap_or_else(|err| panic!("{name} 用 usvg 0.46 解析失败: {err}"));
    }
}
