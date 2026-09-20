//! markdown 渲染里的 mermaid 代码块画图：全仓库唯一入口。
//!
//! `gpui-component` 的 `TextView::markdown` 默认把 mermaid 围栏代码块当普通代码
//! 块处理（纯文本 + 语法高亮壳，mermaid 语法本身没有 tree-sitter 语法，等于纯文
//! 本）。这里接上它的 `markdown_block_parser`/`markdown_block_renderer` 钩子，识
//! 别出 mermaid 代码块，用 `rusty-mermaid`（纯 Rust，无浏览器/Node 依赖）离线渲成
//! SVG，再用 GPUI 的 `img()` 全彩显示——注意不
//! 是 `svg()`：那个元素只做单色 alpha mask 渲染（图标那条路），画不出多彩图，
//! `img()` 对 `.svg` 走的才是全彩栅格化管线。
//!
//! 渲染库换过一次：最早用的 `mermaid-rs-renderer`，实测边路由（尤其反向/回边）
//! 绕远路明显；`rusty-mermaid` 用的是真正的 Sugiyama/dagre 布局算法，同一张图边
//! 路径干净不少。两者都遇到同一个坑：内置字体栈不含 CJK 字体，得自己覆盖（见
//! `MERMAID_LEGACY_FONT_FAMILY` 上的注释）。
//!
//! 全仓库新增/已有的 `TextView::markdown` 调用点都应该改走这里的 `markdown_view`，
//! 不要直接调 `TextView::markdown`，否则等于每处都得重新接一遍这两个钩子。
//!
//! 内存缓存：GPUI 是即时模式 UI，`BlockNode` 的渲染函数每帧 paint 都会重新调用
//! （对照 gpui-component `node.rs` 里 `CODE_BLOCK_HIGHLIGHTERS` 那个 thread_local
//! 先例），同一张图只渲一次，否则滚动会变成每帧重算。不落盘——这是派生图，不是状态。
//!
//! mermaid 渲染库默认输出不透明白底、不会跟随亮暗主题，缓存 key 必须把主题模式
//! 并进去，否则暗色模式下会看到刺眼的白底图。

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use gpui::{
    AnyElement, App, ElementId, HighlightStyle, InteractiveElement, IntoElement, ObjectFit,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, StyledImage, Window, div, img,
    px, relative, rgb,
};
use gpui_component::ActiveTheme;
use gpui_component::text::{
    MarkdownExtensions, MarkdownNode, MarkdownParseContext, TextView, TextViewStyle, markdown_ast,
};
use sha2::{Digest, Sha256};

/// rusty-mermaid（crates.io `=0.2.0`）默认字体栈——见其
/// `rusty_mermaid_core::font_fallback::SVG_FONT_FAMILY` 常量。整份字体栈都是西文
/// 字体（Intel One Mono / SF Mono / .../ monospace），不含任何 CJK 字体，且
/// `Theme`/`SvgConfig` 都没有暴露字体覆盖的公开 API（`Theme.custom_font` 是给
/// raster/gpui 后端吃字体字节用的，SVG 后端根本不读它）——所以只能在渲染出的
/// SVG 文本里做字符串替换。这个值是抄自它当前版本的源码字面量，不是公开契约，
/// 升级 crate 版本前必须先去它源码里确认这个字符串没变，变了这段替换就会静默
/// 失效（不会报错，只是又变回一串画不出字的默认字体名）。
const MERMAID_LEGACY_FONT_FAMILY: &str = "'Intel One Mono', 'SF Mono', 'Cascadia Code', 'JetBrains Mono', 'Fira Code', 'Consolas', 'Menlo', monospace";

/// 换成系统里实测确认存在、且中英文字形都齐的字体，兜底几个常见西文字体。这条
/// 经验是从上一版 mermaid-rs-renderer 那次调试来的：GPUI 的 usvg 字体解析按字面
/// 名字精确匹配 `font-family`，不认 "sans-serif"/"monospace" 这类 CSS 泛型关键
/// 字，字体栈里但凡没有一个字面匹配得上，文字（不分中英文）就整个画不出来。
///
/// 首选没敢直接写 PingFang SC——实测它的字体文件在
/// `/System/Library/PrivateFrameworks/FontServices.framework/.../Reserved/
/// PingFangUI.ttc`，是私有框架资源，不在 `fontdb`（GPUI 底层用的字体库，只走标准
/// 目录扫描）扫描得到的范围内，任何拼法都查不到，不是名字问题。换成冬青黑体简体
/// 中文（Hiragino Sans GB）：PingFang 之前 macOS 用了很多年的中文默认字体，笔画
/// 比黑体-简细、观感上更接近 PingFang，且实测在标准字体目录（会被 fontdb 扫到）。
const MERMAID_FONT_FAMILY: &str =
    "Hiragino Sans GB, Heiti SC, PingFang SC, Helvetica Neue, Arial, sans-serif";

/// 内存缓存 key 版本：改了渲染库/字体栈得跟着升，避免同进程里旧产物被复用。
const CACHE_FORMAT_VERSION: &str = "v4";

/// parser 阶段存进 `MarkdownNode` 的原始数据。parser 不能碰 Window/App，真正的
/// 渲染（调 rusty-mermaid、查缓存）都留到 renderer 阶段做。
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
/// （从 SVG 的 `viewBox` 读出来，不缩放——超宽图靠外层横向滚动兜底，而不是压缩
/// 到看不清文字，见 `render_mermaid_block`）。
struct MermaidImage {
    image: Arc<gpui::Image>,
    width: f32,
    height: f32,
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
/// 不会触发重渲；格式版本必须编进去，否则升级渲染逻辑后旧缓存文件会一直被复用。
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

fn inject_cjk_font_style(svg: &str) -> String {
    let replaced = svg.replace(MERMAID_LEGACY_FONT_FAMILY, MERMAID_FONT_FAMILY);
    const CJK_STYLE: &str = r#"<style>text, tspan { font-family: Hiragino Sans GB, Heiti SC, PingFang SC, Helvetica Neue, Arial, sans-serif !important; }</style>"#;
    if let Some(pos) = replaced.find("<svg")
        && let Some(tag_end) = replaced[pos..].find('>')
    {
        let insert_at = pos + tag_end + 1;
        let mut out = String::with_capacity(replaced.len() + CJK_STYLE.len());
        out.push_str(&replaced[..insert_at]);
        out.push_str(CJK_STYLE);
        out.push_str(&replaced[insert_at..]);
        return out;
    }
    replaced
}

fn render_svg(source: &str, is_dark: bool) -> Result<String, String> {
    let theme = if is_dark {
        rusty_mermaid::Theme::dark()
    } else {
        rusty_mermaid::Theme::light()
    };
    let svg = rusty_mermaid::to_svg(source, &theme).map_err(|e| e.to_string())?;
    Ok(inject_cjk_font_style(&svg))
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
        MermaidRender::Ok(m) => div()
            .id(SharedString::from(format!("mermaid-{digest}")))
            .overflow_x_scroll()
            .child(
                img(m.image.clone())
                    .object_fit(ObjectFit::Contain)
                    .max_w(relative(1.))
                    .w(px(m.width))
                    .h(px(m.height)),
            )
            .into_any_element(),
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
                    .font_family(smelt_core::font_config::font_family())
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
    let mut style = TextViewStyle::default().inline_code(inline_code);
    style.is_dark = !crate::ui_theme::is_light();
    style
}

fn markdown_view_with_selection(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
    selectable: bool,
) -> TextView {
    TextView::markdown(id, text)
        .selectable(selectable)
        .style(markdown_text_view_style())
        .markdown_extensions(shared_markdown_extensions().clone())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_extensions_are_reused_across_views() {
        assert!(std::ptr::eq(
            shared_markdown_extensions(),
            shared_markdown_extensions()
        ));
    }

    #[test]
    fn valid_source_renders_without_writing_disk() {
        let img = render_or_load("flowchart TD\nA-->B", false).expect("应能渲染");
        assert!(img.width > 0.0 && img.height > 0.0);
        let svg = render_svg("flowchart TD\nA-->B", false).expect("应能渲出 SVG");
        assert!(svg.trim_start().starts_with("<svg"));
    }

    /// 回归守卫：rusty-mermaid 内置字体栈在 GPUI 的 usvg 字体解析里一个都对不上
    /// （实测验证过，见 `MERMAID_LEGACY_FONT_FAMILY`/`MERMAID_FONT_FAMILY` 上的
    /// 注释），会导致文字整个画不出来（不分中英文，不只是缺中文字形）。这里锁死
    /// 「渲染产物必须带上覆盖后的字体名」，防止以后有人顺手把 `render_or_load`
    /// 里那行字符串替换删掉。
    #[test]
    fn rendered_svg_overrides_font_family_to_a_resolvable_one() {
        let img = render_or_load("flowchart TD\nA[开始]-->B[结束]", false).expect("应能渲染");
        let svg = render_svg("flowchart TD\nA[开始]-->B[结束]", false).unwrap();
        assert!(
            svg.contains("Hiragino Sans GB"),
            "渲染产物应该用覆盖后的字体栈，而不是渲染库默认的那套解析不出来的西文字体名"
        );
        assert!(img.width > 0.0 && img.height > 0.0);
    }

    #[test]
    fn invalid_source_errs_without_panicking() {
        assert!(render_or_load("this is not mermaid at all {{{", false).is_err());
    }

    #[test]
    fn theme_mode_changes_cache_key() {
        let light = source_digest("graph TD\nA-->B", false);
        let dark = source_digest("graph TD\nA-->B", true);
        assert_ne!(light, dark);
        render_or_load("graph TD\nA-->B", false).expect("亮色应能渲染");
        render_or_load("graph TD\nA-->B", true).expect("暗色应能渲染");
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
    fn inject_cjk_font_style_inserts_css_rule() {
        let raw = "<svg width=\"100\" height=\"100\"><g></g></svg>";
        let injected = super::inject_cjk_font_style(raw);
        assert!(injected.contains("<style>text, tspan { font-family:"));
        assert!(injected.starts_with("<svg width=\"100\" height=\"100\"><style>"));
    }
}
