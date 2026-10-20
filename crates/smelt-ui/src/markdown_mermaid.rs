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
//! 全仓库新增/已有的 `TextView::markdown` 调用点都应该改走这里的 `markdown_view`，
//! 不要直接调 `TextView::markdown`，否则等于每处都得重新接一遍这两个钩子。
//!
//! 内存缓存：GPUI 是即时模式 UI，`BlockNode` 的渲染函数每帧 paint 都会重新调用
//! （对照 gpui-component `node.rs` 里 `CODE_BLOCK_HIGHLIGHTERS` 那个 thread_local
//! 先例），同一张图只渲一次，否则滚动会变成每帧重算。不落盘——这是派生图，不是状态。

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
    MarkdownExtensions, MarkdownNode, MarkdownParseContext, TextView, TextViewState, TextViewStyle,
    markdown_ast,
};
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
fn strip_negative_stroke_width(svg: &str) -> String {
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

/// 系统字体库里拷出来的一个面。字节按 mmap 指针去重，同一 TTC 的 regular / bold
/// 共用一块 `Arc`。
struct LoadedFace {
    family: String,
    data: Arc<[u8]>,
    index: u32,
    bold: bool,
}

fn measure_faces() -> &'static [LoadedFace] {
    static FACES: OnceLock<Vec<LoadedFace>> = OnceLock::new();
    FACES.get_or_init(load_measure_faces)
}

/// GPUI 的 `SvgRenderer` 用 `usvg::fontdb::Database::load_system_fonts()` 扫系统字体
/// （另外还会塞 IBM Plex / Lilex，只用来兜泛型家族）。gpui 不把这份 fontdb 公开出来，
/// 所以这里直接依赖同一个 usvg 0.46，按 SVG 里那条字体栈查询，再退到 sans-serif、
/// 任意一款，以及 Linux 上常见的 CJK 字体。
fn load_measure_faces() -> Vec<LoadedFace> {
    let mut db = usvg::fontdb::Database::new();
    db.load_system_fonts();
    let mut faces = Vec::new();
    let mut blobs: HashMap<usize, Arc<[u8]>> = HashMap::new();
    let mut seen: HashSet<(usize, u32)> = HashSet::new();

    for name in font_family_names(MERMAID_FONT_FAMILY) {
        if is_generic_family(&name) {
            continue;
        }
        push_named_family(&db, &name, &mut faces, &mut blobs, &mut seen);
    }
    push_queried_family(
        &db,
        "sans-serif",
        query_sans(&db, 400),
        query_sans(&db, 700),
        &mut faces,
        &mut blobs,
        &mut seen,
    );
    if !faces_cover(&faces, '中') {
        for name in CJK_MEASURE_FALLBACKS {
            push_named_family(&db, name, &mut faces, &mut blobs, &mut seen);
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
            push_face(&db, id, "fallback", bold, &mut faces, &mut blobs, &mut seen);
        }
    }
    faces
}

fn push_named_family(
    db: &usvg::fontdb::Database,
    family: &str,
    faces: &mut Vec<LoadedFace>,
    blobs: &mut HashMap<usize, Arc<[u8]>>,
    seen: &mut HashSet<(usize, u32)>,
) {
    push_queried_family(
        db,
        family,
        query_named(db, family, 400),
        query_named(db, family, 700),
        faces,
        blobs,
        seen,
    );
}

fn push_queried_family(
    db: &usvg::fontdb::Database,
    family: &str,
    regular: Option<usvg::fontdb::ID>,
    bold: Option<usvg::fontdb::ID>,
    faces: &mut Vec<LoadedFace>,
    blobs: &mut HashMap<usize, Arc<[u8]>>,
    seen: &mut HashSet<(usize, u32)>,
) {
    if let Some(id) = regular {
        let weight = db.face(id).map(|face| face.weight.0).unwrap_or(400);
        push_face(db, id, family, weight >= 600, faces, blobs, seen);
    }
    if let Some(id) = bold {
        let weight = db.face(id).map(|face| face.weight.0).unwrap_or(400);
        if weight >= 600 {
            push_face(db, id, family, true, faces, blobs, seen);
        }
    }
}

fn push_face(
    db: &usvg::fontdb::Database,
    id: usvg::fontdb::ID,
    family: &str,
    bold: bool,
    faces: &mut Vec<LoadedFace>,
    blobs: &mut HashMap<usize, Arc<[u8]>>,
    seen: &mut HashSet<(usize, u32)>,
) {
    let Some((ptr, index)) = db.with_face_data(id, |data, index| (data.as_ptr() as usize, index))
    else {
        return;
    };
    if !seen.insert((ptr, index)) {
        return;
    }
    let data = if let Some(existing) = blobs.get(&ptr) {
        Arc::clone(existing)
    } else {
        let Some(copied) = db.with_face_data(id, |data, _| Arc::<[u8]>::from(data)) else {
            return;
        };
        blobs.insert(ptr, Arc::clone(&copied));
        copied
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
    rustybuzz::Face::from_slice(&face.data, face.index)
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
    let parsed: Vec<Option<rustybuzz::Face<'_>>> = faces
        .iter()
        .map(|face| rustybuzz::Face::from_slice(&face.data, face.index))
        .collect();
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
        let idx = pick_face(&parsed, faces, ch, &preferred, bold);
        if started && run_face != idx {
            total += run_width(&parsed, faces, run_face, &run, font_size, bold);
            run.clear();
        }
        started = true;
        run_face = idx;
        run.push(ch);
    }
    total += run_width(&parsed, faces, run_face, &run, font_size, bold);
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
    with_markdown_hooks(TextView::markdown(id, text).selectable(selectable))
}

/// 和 [`markdown_view_clickable`] 一样的 Markdown，只是解析状态由调用方持有。
///
/// `TextView::markdown` 把解析结果挂在元素上，元素有一帧没画就丢掉。虚拟列表里的行
/// 滚出渲染范围再回来，超过 4KB 的正文要先以空内容等后台重新解析，行高跟着跳。
/// 这类地方自己留住 `TextViewState`，用这个入口画。
pub fn markdown_view_for_state(state: &gpui::Entity<TextViewState>) -> TextView {
    with_markdown_hooks(TextView::new(state).selectable(true))
}

fn with_markdown_hooks(view: TextView) -> TextView {
    view.style(markdown_text_view_style())
        .code_block_actions(code_block_corner)
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
        let raw = "#mm .edge-depth-5{stroke-width:-1;}#mm .edge{stroke-width:3;}<line stroke-width=\"-4\" stroke=\"red\"/>";
        let fixed = super::strip_negative_stroke_width(raw);
        assert_eq!(
            fixed,
            "#mm .edge-depth-5{}#mm .edge{stroke-width:3;}<line stroke=\"red\"/>"
        );
        assert!(!fixed.contains("stroke-width:-"));
        assert!(fixed.contains("stroke-width:3"));
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

    /// gpui-pre 0.3.9 栅格化 SVG 用的就是 usvg 0.46。这里用同一份 crate 解析，
    /// 确认 `resvg_safe` 的输出不会被这版 usvg 拒掉。
    fn assert_gpui_usvg_parses(name: &str, svg: &str) {
        let options = usvg::Options::default();
        usvg::Tree::from_str(svg, &options)
            .unwrap_or_else(|err| panic!("{name} 用 usvg 0.46 解析失败: {err}"));
    }
}
