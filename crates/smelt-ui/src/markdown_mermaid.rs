//! 纯 Rust Mermaid 图表 SVG 渲染引擎（全仓库唯一入口，供 NSH `<SmeltMarkdown>` 经宿主 `mermaid.render` 调用）。
//!
//! 用 `merman`（纯 Rust，对齐 mermaid.js）离线渲成全彩 SVG data URI，背景透明并跟随当前 Smelt 色板
//! （见 `theme.rs`）。类图标签包在带 `overflow:hidden` 的嵌套 svg 里，测宽回调使用与 GPUI/NSH 一致的
//! `usvg 0.46` 系统 fontdb + `rustybuzz` 量真实字形 advance，防止中文与粗体标签被裁切。
//!
//! 不编 `layout-elk`（带入 EPL-2.0 许可且改默认布局为 ELK）；`elk` 请求自动退回 dagre。

use rust_i18n::t;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use merman::svg::{
    DeterministicTextMeasurer, MeasurementProfileId, SvgPipeline, TextMeasurementPolicy,
    TextMeasurementProfile, TextMeasurementProfileIdentity, TextStyle,
};
use merman::{
    Engine, MermaidConfig, OperationControl, RenderOutput, RenderRequest, Renderer, SvgEnvironment,
    SvgRequest,
};
use sha2::{Digest, Sha256};

mod theme;

use theme::DiagramColors;

/// 传给 merman `fontFamily` 的 CSS 字体栈。多词名字必须加引号，否则 CSS 会把
/// `Hiragino Sans GB` 拆成三个 family。usvg 按字面名精确匹配 `font-family`。
const MERMAID_FONT_FAMILY: &str =
    "\"Hiragino Sans GB\", \"Heiti SC\", \"PingFang SC\", \"Helvetica Neue\", Arial, sans-serif";

/// 机器上没有上面这套字体时，测宽再试的 CJK 家族（只参与量字宽，不写进 SVG）。
const CJK_MEASURE_FALLBACKS: &[&str] = &[
    "Noto Sans CJK SC",
    "Noto Sans CJK JP",
    "Noto Sans SC",
    "Source Han Sans SC",
    "WenQuanYi Micro Hei",
    "Droid Sans Fallback",
];

/// 内存缓存 key 版本：换了渲染库、字体栈或 SVG 后处理就跟着升。
const CACHE_FORMAT_VERSION: &str = "v6";

/// 粗体没有独立字重时，在 regular advance 上乘的系数。
const SYNTHETIC_BOLD_SCALE: f64 = 1.06;

const MERMAID_URI_CACHE_MAX: usize = 256;

/// 供 NSH 后台线程消费的纯 Rust Mermaid 渲染结果：全彩 SVG data URI 与自然宽高。
#[derive(Debug, Clone, PartialEq)]
pub struct MermaidSvgDataUri {
    pub data_uri: String,
    pub width: f32,
    pub height: f32,
}

/// 线程安全、带进程内存缓存的纯 Rust Mermaid SVG 渲染入口（供 NSH 插件 `<SmeltMarkdown>` 使用）。
/// 渲染失败返回错误文案；同一 `(源码, 当前主题色板)` 命中缓存直接 O(1) 返回。
pub fn render_mermaid_svg_data_uri(source: &str) -> Result<MermaidSvgDataUri, String> {
    use base64::Engine as _;

    struct Cache {
        map: HashMap<String, Result<MermaidSvgDataUri, String>>,
        order: VecDeque<String>,
    }

    static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(|| {
        Mutex::new(Cache {
            map: HashMap::new(),
            order: VecDeque::new(),
        })
    });

    let colors = DiagramColors::current();
    let digest = source_digest(source, &colors);
    {
        let cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(hit) = cache.map.get(&digest) {
            return hit.clone();
        }
    }

    let rendered = render_svg(source, colors).map(|svg| {
        let (width, height) = parse_viewbox_size(&svg).unwrap_or((400.0, 300.0));
        let b64 = base64::engine::general_purpose::STANDARD.encode(svg.as_bytes());
        MermaidSvgDataUri {
            data_uri: format!("data:image/svg+xml;base64,{b64}"),
            width,
            height,
        }
    });

    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if !cache.map.contains_key(&digest) {
        while cache.map.len() >= MERMAID_URI_CACHE_MAX {
            if let Some(evicted) = cache.order.pop_front() {
                cache.map.remove(&evicted);
            } else {
                break;
            }
        }
        cache.order.push_back(digest.clone());
        cache.map.insert(digest, rendered.clone());
    }
    rendered
}

/// `sha256(缓存格式版本 + 源码 + 配色)`。配色必须编进去，否则换主题、切亮暗不会触发重渲。
fn source_digest(source: &str, colors: &DiagramColors) -> String {
    let mut hasher = Sha256::new();
    hasher.update(CACHE_FORMAT_VERSION.as_bytes());
    hasher.update(source.as_bytes());
    hasher.update(b"\0");
    colors.feed(&mut hasher);
    hex::encode(hasher.finalize())
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

/// mindmap 按 `17 - 3*i` 给深层 section 写 `stroke-width`，到 `edge-depth-5` 是 `-1`。
/// resvg / usvg 见到负数会把这条线整段丢掉；这里只清理标签属性与 `<style>` 中的负声明，不动文字节点。
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
        if tag.starts_with("<style") && !tag.ends_with("/>") {
            let end = rest.find("</style>").unwrap_or(rest.len());
            out.push_str(&strip_negative_stroke_width_in(&rest[..end]));
            rest = &rest[end..];
        }
    }
    out.push_str(rest);
    out
}

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

fn parsed_faces() -> &'static [Option<rustybuzz::Face<'static>>] {
    static PARSED: OnceLock<Vec<Option<rustybuzz::Face<'static>>>> = OnceLock::new();
    PARSED.get_or_init(|| {
        measure_faces()
            .iter()
            .map(|face| rustybuzz::Face::from_slice(face.bytes(), face.index))
            .collect()
    })
}

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
    // SAFETY: 映射的是系统字体文件，进程里一直留着；与 GPUI / usvg 栅格化 SVG 映射方式一致。
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

fn renderer(colors: DiagramColors) -> Arc<Renderer> {
    static RENDERERS: LazyLock<Mutex<HashMap<DiagramColors, Arc<Renderer>>>> =
        LazyLock::new(Default::default);
    let mut renderers = RENDERERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    renderers
        .entry(colors)
        .or_insert_with(|| {
            let config = MermaidConfig::from_value(colors.site_config(MERMAID_FONT_FAMILY));
            Arc::new(Renderer::new().with_engine(Engine::new().with_site_config(config)))
        })
        .clone()
}

fn svg_request() -> SvgRequest {
    SvgRequest {
        pipeline: Some(SvgPipeline::resvg_safe()),
        environment: SvgEnvironment::deterministic()
            .with_text_measurement_policy(text_measurement_policy().clone()),
        ..Default::default()
    }
}

fn render_svg(source: &str, colors: DiagramColors) -> Result<String, String> {
    match std::panic::catch_unwind(|| render_svg_inner(source, colors)) {
        Ok(result) => result,
        Err(_) => Err(t!("mermaid.panicked").into_owned()),
    }
}

fn render_svg_inner(source: &str, colors: DiagramColors) -> Result<String, String> {
    let output = renderer(colors)
        .render(RenderRequest::svg(
            source,
            OperationControl::new(),
            svg_request(),
        ))
        .map_err(|err| err.to_string())?;
    match output {
        RenderOutput::Svg(Some(svg)) => Ok(strip_negative_stroke_width(svg.svg())),
        RenderOutput::Svg(None) => Err(t!("mermaid.no_diagram").into_owned()),
        _ => Err(t!("mermaid.not_svg").into_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn colors(dark: bool) -> DiagramColors {
        DiagramColors::from_palette(&crate::ui_theme::builtin_fallback(!dark), !dark)
    }

    #[test]
    fn render_mermaid_svg_data_uri_produces_valid_data_uri_and_caches() {
        let first = render_mermaid_svg_data_uri("flowchart TD\nA-->B").expect("应能渲染");
        assert!(first.data_uri.starts_with("data:image/svg+xml;base64,"));
        assert!(first.width > 0.0 && first.height > 0.0);
        let second = render_mermaid_svg_data_uri("flowchart TD\nA-->B").expect("应命中缓存");
        assert_eq!(first, second);
    }

    #[test]
    fn rendered_svg_uses_cjk_font_stack() {
        let svg = render_svg("flowchart TD\nA[开始]-->B[结束]", colors(false)).unwrap();
        assert!(svg.contains("Hiragino Sans GB"));
        let (w, h) = parse_viewbox_size(&svg).expect("应有正 viewBox");
        assert!(w > 0.0 && h > 0.0);
    }

    #[test]
    fn invalid_source_errs_without_panicking() {
        assert!(render_svg("this is not mermaid at all {{{", colors(false)).is_err());
    }

    #[test]
    fn theme_mode_changes_cache_key_and_svg() {
        let light = source_digest("graph TD\nA-->B", &colors(false));
        let dark = source_digest("graph TD\nA-->B", &colors(true));
        assert_ne!(light, dark);
        let light_svg = render_svg("graph TD\nA-->B", colors(false)).expect("亮色应能渲染");
        let dark_svg = render_svg("graph TD\nA-->B", colors(true)).expect("暗色应能渲染");
        assert_ne!(light_svg, dark_svg);
    }

    #[test]
    fn parse_viewbox_size_handles_single_quotes_and_whitespace() {
        assert_eq!(
            parse_viewbox_size("<svg viewBox='0 0 500 300'></svg>"),
            Some((500.0, 300.0))
        );
        assert_eq!(
            parse_viewbox_size("<svg viewBox=\" 0  0   800.5  600.25 \"></svg>"),
            Some((800.5, 600.25))
        );
        assert_eq!(parse_viewbox_size("<svg viewBox=\"0 0 -10 20\"></svg>"), None);
    }

    #[test]
    fn strip_negative_stroke_width_keeps_positive_rules_and_leaves_text_alone() {
        let raw = "<svg><style>#mm .edge-depth-5{stroke-width:-1;}#mm .edge{stroke-width:3;}</style><line stroke-width=\"-4\" stroke=\"red\"/></svg>";
        assert_eq!(
            strip_negative_stroke_width(raw),
            "<svg><style>#mm .edge-depth-5{}#mm .edge{stroke-width:3;}</style><line stroke=\"red\"/></svg>"
        );
        let raw2 = "<svg><style><![CDATA[.a > .b{stroke-width:-2}]]></style><text x=\"1\" style=\"stroke-width:-3\">stroke-width:-1</text></svg>";
        assert_eq!(
            strip_negative_stroke_width(raw2),
            "<svg><style><![CDATA[.a > .b{}]]></style><text x=\"1\" style=\"\">stroke-width:-1</text></svg>"
        );
    }

    #[test]
    fn font_family_names_keep_quoted_spaces() {
        assert_eq!(
            font_family_names(MERMAID_FONT_FAMILY),
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

    #[test]
    fn samples_render_svg_that_usvg_parses() {
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
                let svg = render_svg(source, colors(dark))
                    .unwrap_or_else(|err| panic!("{name} dark={dark} 渲染失败: {err}"));
                assert!(svg.trim_start().starts_with("<svg"), "{name}");
                let (w, h) = parse_viewbox_size(&svg).unwrap_or_else(|| panic!("{name} viewBox"));
                assert!(w > 0.0 && h > 0.0);
                assert!(!svg.contains("stroke-width:-"));
                usvg::Tree::from_str(&svg, &usvg::Options::default())
                    .unwrap_or_else(|err| panic!("{name} usvg 解析失败: {err}"));
            }
        }
    }

    #[test]
    fn flowcharts_lay_out_with_dagre_even_when_elk_is_requested() {
        let body = "flowchart TD\nA[开始] --> B[处理]\nB --> C[结束]\nC --> A\n";
        let elk_src = format!("---\nconfig:\n  layout: elk\n---\n{body}");
        let dagre_src = format!("---\nconfig:\n  layout: dagre\n---\n{body}");

        let plain = render_svg(body, colors(false)).expect("默认布局应能渲染");
        let elk = render_svg(&elk_src, colors(false)).expect("layout: elk 应退回 dagre");
        let dagre = render_svg(&dagre_src, colors(false)).expect("layout: dagre 应能渲染");
        usvg::Tree::from_str(&elk, &usvg::Options::default()).expect("usvg 解析");

        let dagre_box = parse_viewbox_size(&dagre).expect("dagre viewBox");
        assert_eq!(parse_viewbox_size(&plain), Some(dagre_box));
        assert_eq!(parse_viewbox_size(&elk), Some(dagre_box));
    }
}
