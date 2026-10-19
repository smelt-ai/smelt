//! 图片显示统一抽象：解码 + 降采样 + 按图片 id 缓存 + contain 绘制。
//!
//! 背景：ACP 图片显示反复出 bug——大图撑破屏幕、长截图只露顶部一截、超高图撞
//! sprite atlas 上限（16384px）画不出来。先后四次修复（「盒子 min_w/min_h 归
//! 零」→「预览改 absolute+inset」→「viewport_size 算显式尺寸」→「canvas 手动
//! contain + 降采样」）都在调用点各打各的补丁，第五次是迟早的事。这里把「图片
//! → 受控尺寸的 RenderImage → contain 绘制」收敛成唯一路径，调用点只描述「显
//! 示哪张图、盒子多大」：
//!
//! - 解码/降采样在后台线程执行，结果按 `gpui::Image::id` 缓存（建图时算一次，
//!   同一张图多处显示只解一次；绘制路径不再对字节做哈希）
//! - 大图降到 `MAX_PREVIEW_DIM` 以内，避免撞 sprite atlas 上限
//! - 超出条数时先记下被淘汰的 `RenderImage`，回到前台再 `drop_image`。atlas
//!   按 `RenderImage` 的递增 id 留纹理，只丢掉 CPU 侧的 `Arc` 不会释放显存
//! - 绘制用 canvas 手动 contain，不依赖 taffy 的 aspect_ratio 推导（那是历次
//!   「撑破屏幕 / 只露顶部」的根因）
//!
//! 消息流内嵌缩略图、点击大图预览、文件预览、壁纸都应走这里，不要各自写
//! `img()` + 尺寸补丁。`Svg` 不走降采样：gpui 的 `img()` 对 `.svg` 走全彩栅格化
//! 管线，本模块的 image crate 不解码 SVG，调用点对 SVG 保留原 `img()` 即可。

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use gpui::*;

/// 降采样后的最大边长。gpui sprite atlas 上限 16384px，这里留足余量；
/// 长截图几万像素高时仍能完整画出来。
pub const MAX_PREVIEW_DIM: u32 = 2048;

/// 缓存条数上限：图片消息流可能很长，按图片 id 的缓存要防内存无限增长。
/// 超出后按 FIFO 淘汰最旧条目。
const CACHE_CAPACITY: usize = 64;

/// 一次解码的结局。失败也留下，避免坏图在每一帧被重新拿去解码。
enum StoredPreview {
    Ready(Arc<RenderImage>),
    Failed,
}

struct ImageCacheInner {
    entries: HashMap<u64, StoredPreview>,
    order: VecDeque<u64>,
    /// 正在后台解码的图片 id。和缓存放在同一把锁里，绘制路径先查再决定要不要启动。
    inflight: HashSet<u64>,
    /// 已从缓存摘下、还占着 sprite atlas 的预览。只在前台排空。
    pending_drops: Vec<Arc<RenderImage>>,
}

fn image_cache() -> &'static Mutex<ImageCacheInner> {
    static CACHE: OnceLock<Mutex<ImageCacheInner>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(ImageCacheInner {
            entries: HashMap::new(),
            order: VecDeque::new(),
            inflight: HashSet::new(),
            pending_drops: Vec::new(),
        })
    })
}

fn cached_render(id: u64) -> Option<Arc<RenderImage>> {
    match image_cache().lock().unwrap().entries.get(&id) {
        Some(StoredPreview::Ready(render)) => Some(Arc::clone(render)),
        _ => None,
    }
}

/// gpui::ImageFormat → image crate 的 ImageFormat。SVG 走 gpui 的 svg_renderer，
/// image crate 不解码，返回 None。
fn to_image_format(f: gpui::ImageFormat) -> Option<image::ImageFormat> {
    Some(match f {
        gpui::ImageFormat::Png => image::ImageFormat::Png,
        gpui::ImageFormat::Jpeg => image::ImageFormat::Jpeg,
        gpui::ImageFormat::Webp => image::ImageFormat::WebP,
        gpui::ImageFormat::Gif => image::ImageFormat::Gif,
        gpui::ImageFormat::Bmp => image::ImageFormat::Bmp,
        gpui::ImageFormat::Tiff => image::ImageFormat::Tiff,
        gpui::ImageFormat::Ico => image::ImageFormat::Ico,
        gpui::ImageFormat::Pnm => image::ImageFormat::Pnm,
        gpui::ImageFormat::Svg => return None,
    })
}

/// 同步命中：这张图已经降采样过就返回预览。还在解码、解不开、或根本没开始时
/// 返回 None。栅格缩略图这时只能占位，不能把原图交给 `img()`；见 [`thumb_paint`]。
pub fn cached(image: &gpui::Image) -> Option<Arc<RenderImage>> {
    cached_render(image.id)
}

/// 栅格图还没有结局（既没有预览，也没有记下失败）。SVG 不算，它不走这条解码。
/// 绘制路径用它决定要不要转占位动画；真正启动解码用 [`ensure_preview`]。
pub fn preview_pending(image: &gpui::Image) -> bool {
    if image.format == gpui::ImageFormat::Svg {
        return false;
    }
    !image_cache()
        .lock()
        .unwrap()
        .entries
        .contains_key(&image.id)
}

/// 还没人解码这张图时占住在途标记。已有预览、已经失败、或已有任务在飞时返回 false。
/// 必须在 `spawn` 之前调用：画一帧查一次，不能等任务里再发现「已经在跑」然后通知重绘。
fn begin_preview(image: &gpui::Image) -> bool {
    if image.format == gpui::ImageFormat::Svg {
        return false;
    }
    let mut cache = image_cache().lock().unwrap();
    if cache.entries.contains_key(&image.id) {
        return false;
    }
    cache.inflight.insert(image.id)
}

fn finish_preview(id: u64, render: Option<Arc<RenderImage>>) {
    let mut cache = image_cache().lock().unwrap();
    store_preview(
        &mut cache,
        id,
        match render {
            Some(render) => StoredPreview::Ready(render),
            None => StoredPreview::Failed,
        },
    );
}

/// 写入一条预览并按 FIFO 淘汰。淘汰下来的已解码图片留在 `pending_drops`，
/// 调用方稍后在前台交给 `drop_image`。失败记录没有上传 atlas，直接丢掉。
fn store_preview(cache: &mut ImageCacheInner, id: u64, stored: StoredPreview) {
    cache.inflight.remove(&id);
    if cache.entries.contains_key(&id) {
        return;
    }
    cache.entries.insert(id, stored);
    cache.order.push_back(id);
    while cache.order.len() > CACHE_CAPACITY {
        let Some(oldest) = cache.order.pop_front() else {
            break;
        };
        if let Some(StoredPreview::Ready(render)) = cache.entries.remove(&oldest) {
            cache.pending_drops.push(render);
        }
    }
}

fn take_pending_drops() -> Vec<Arc<RenderImage>> {
    let mut cache = image_cache().lock().unwrap();
    std::mem::take(&mut cache.pending_drops)
}

/// 释放上一轮淘汰仍占着 sprite atlas 的纹理。必须在前台调用：`drop_image`
/// 要拿到窗口。后台解码只能先把 `RenderImage` 留在 `pending_drops`。
fn release_dropped_previews(cx: &mut App) {
    for image in take_pending_drops() {
        cx.drop_image(image, None);
    }
}

fn put(id: u64, render: Arc<RenderImage>) {
    finish_preview(id, Some(render));
}

/// 栅格图还没有预览时，在后台解码一次，完成后刷新窗口。
///
/// 绘制路径可以每帧调用：已在途、已完成、已失败或是 SVG 时直接返回，不再启动任务。
/// 解码结果按图片 id 放进进程内缓存，所有显示这张图的视图下一帧一起看到。
pub fn ensure_preview(image: &Arc<gpui::Image>, cx: &App) {
    if !begin_preview(image) {
        return;
    }
    let image = Arc::clone(image);
    cx.spawn(async move |cx| {
        let id = image.id;
        let decoded = cx
            .background_executor()
            .spawn(async move { decode_preview(&image) })
            .await;
        finish_preview(id, decoded);
        cx.update(release_dropped_previews);
        cx.refresh();
    })
    .detach();
}

/// 解码 + 降采样成 RenderImage（BGRA）供 canvas 直接绘制。
///
/// 顺带把 RGBA 字节换成 gpui 纹理要的 BGRA。失败（格式不支持 / 数据损坏）返回
/// None，调用点留空即可，不阻塞 UI。解码结果按图片 id 放进 `cached()` 的
/// 缓存，同一张图多处显示只解一次。
pub fn decode_preview(image: &gpui::Image) -> Option<Arc<RenderImage>> {
    let format = to_image_format(image.format)?;
    let decoded = image::load_from_memory_with_format(&image.bytes, format).ok()?;
    let (w, h) = (decoded.width(), decoded.height());
    let scale = (MAX_PREVIEW_DIM as f32 / w as f32)
        .min(MAX_PREVIEW_DIM as f32 / h as f32)
        .min(1.0);
    let img = if scale < 1.0 {
        decoded.resize(
            ((w as f32) * scale) as u32,
            ((h as f32) * scale) as u32,
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        decoded
    };
    let mut rgba = img.into_rgba8();
    // RGBA → BGRA：gpui 纹理格式。
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    let frame = image::Frame::new(rgba);
    Some(Arc::new(RenderImage::new(vec![frame])))
}

/// 缩略图该画哪一份。栅格图只允许降采样预览；未完成时是占位，调用方不得改画原图。
#[derive(Clone)]
pub enum ThumbPaint {
    /// 长边已收到 [`MAX_PREVIEW_DIM`] 以内的预览。
    Preview(Arc<RenderImage>),
    /// 栅格图还在解码。画固定盒子，不要把原图交给 `img()`。
    Placeholder,
    /// SVG 没有 image crate 的解码，仍走 gpui 的全彩栅格化。
    Svg,
}

/// 栅格缩略图的绘制选择。`img(原图)` 会按文件像素解码并上传 atlas，
/// 这块贴图不随气泡卸掉，所以未入缓存时只能是占位。
pub fn thumb_paint(image: &gpui::Image) -> ThumbPaint {
    if image.format == gpui::ImageFormat::Svg {
        return ThumbPaint::Svg;
    }
    match cached(image) {
        Some(render) => ThumbPaint::Preview(render),
        None => ThumbPaint::Placeholder,
    }
}

/// 按 [`thumb_paint`] 画出缩略图。高度和最大宽度由调用方给定。
pub fn thumb_element(
    image: &Arc<gpui::Image>,
    height: Pixels,
    max_width: Pixels,
    corner_radius: f32,
) -> AnyElement {
    match thumb_paint(image) {
        ThumbPaint::Preview(render) => {
            contain_canvas_at_height(render, height, max_width, corner_radius).into_any_element()
        }
        ThumbPaint::Placeholder => div()
            .h(height)
            .w(height.min(max_width))
            .rounded(px(corner_radius))
            .bg(rgba(0x00000014))
            .into_any_element(),
        ThumbPaint::Svg => img(image.clone())
            .h(height)
            .max_w(max_width)
            .into_any_element(),
    }
}

/// 文件铺进盒子的方式。和 `object_fit` 的 contain / cover 相同，像素来自降采样预览。
#[derive(Clone, Copy)]
pub enum FileFit {
    /// 完整放进盒子，留白。文件预览用这个。
    Contain,
    /// 铺满盒子，多出来的裁掉。壁纸用这个。
    Cover,
}

/// 磁盘上的图该画哪一份。栅格图只允许降采样预览。
enum FilePaint {
    Preview(Arc<RenderImage>),
    /// 还没解码，或者缓存里的预览已被淘汰。
    Placeholder,
    Svg,
    /// 读不到或解不开。同一份文件不再重试。
    Failed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
}

enum PathOutcome {
    Ready(u64),
    Failed,
}

struct PathSlot {
    stamp: Option<FileStamp>,
    outcome: PathOutcome,
}

fn path_slots() -> &'static Mutex<HashMap<PathBuf, PathSlot>> {
    static SLOTS: OnceLock<Mutex<HashMap<PathBuf, PathSlot>>> = OnceLock::new();
    SLOTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn path_inflight() -> &'static Mutex<HashSet<PathBuf>> {
    static IN_FLIGHT_PATHS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    IN_FLIGHT_PATHS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// 路径扩展名到 GPUI 图片格式。会话图片和剪贴板粘贴共用这一张表。
pub fn format_for_path(path: &Path) -> Option<gpui::ImageFormat> {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())?;
    Some(match ext.as_str() {
        "png" => gpui::ImageFormat::Png,
        "jpg" | "jpeg" => gpui::ImageFormat::Jpeg,
        "webp" => gpui::ImageFormat::Webp,
        "gif" => gpui::ImageFormat::Gif,
        "bmp" => gpui::ImageFormat::Bmp,
        "tif" | "tiff" => gpui::ImageFormat::Tiff,
        "svg" => gpui::ImageFormat::Svg,
        _ => return None,
    })
}

fn file_stamp(path: &Path) -> Option<FileStamp> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    Some(FileStamp {
        len: meta.len(),
        modified: meta.modified().ok(),
    })
}

fn remember_path(path: &Path, stamp: Option<FileStamp>, outcome: PathOutcome) {
    path_slots()
        .lock()
        .unwrap()
        .insert(path.to_path_buf(), PathSlot { stamp, outcome });
}

fn file_paint(path: &Path) -> FilePaint {
    if format_for_path(path) == Some(gpui::ImageFormat::Svg) {
        return FilePaint::Svg;
    }
    let stamp = file_stamp(path);
    let hash = {
        let slots = path_slots().lock().unwrap();
        let Some(slot) = slots.get(path) else {
            return FilePaint::Placeholder;
        };
        if slot.stamp != stamp {
            return FilePaint::Placeholder;
        }
        match slot.outcome {
            PathOutcome::Failed => return FilePaint::Failed,
            PathOutcome::Ready(hash) => hash,
        }
    };
    match cached_render(hash) {
        Some(render) => FilePaint::Preview(render),
        None => FilePaint::Placeholder,
    }
}

/// 读文件、降采样、写入和缩略图共用的缓存。认不出或解不开就记失败，不回退 `img()`。
fn load_file_preview(path: &Path) -> FilePaint {
    if format_for_path(path) == Some(gpui::ImageFormat::Svg) {
        return FilePaint::Svg;
    }
    let Some(stamp) = file_stamp(path) else {
        remember_path(path, None, PathOutcome::Failed);
        return FilePaint::Failed;
    };
    let Some(format) = format_for_path(path).filter(|format| *format != gpui::ImageFormat::Svg)
    else {
        remember_path(path, Some(stamp), PathOutcome::Failed);
        return FilePaint::Failed;
    };
    let Ok(bytes) = std::fs::read(path) else {
        remember_path(path, Some(stamp), PathOutcome::Failed);
        return FilePaint::Failed;
    };
    let image = gpui::Image::from_bytes(format, bytes);
    match decode_preview(&image) {
        Some(render) => {
            put(image.id, Arc::clone(&render));
            remember_path(path, Some(stamp), PathOutcome::Ready(image.id));
            FilePaint::Preview(render)
        }
        None => {
            remember_path(path, Some(stamp), PathOutcome::Failed);
            FilePaint::Failed
        }
    }
}

/// 这一帧要不要再排队解码。已有预览、已经失败、或已有任务在飞时返回 false。
fn begin_file_fetch(path: &Path) -> bool {
    if format_for_path(path) == Some(gpui::ImageFormat::Svg) {
        return false;
    }
    let stamp = file_stamp(path);
    {
        let slots = path_slots().lock().unwrap();
        if let Some(slot) = slots.get(path)
            && slot.stamp == stamp
        {
            match slot.outcome {
                PathOutcome::Failed => return false,
                PathOutcome::Ready(hash)
                    if image_cache().lock().unwrap().entries.contains_key(&hash) =>
                {
                    return false;
                }
                PathOutcome::Ready(_) => {}
            }
        }
    }
    path_inflight().lock().unwrap().insert(path.to_path_buf())
}

async fn fetch_file_preview(path: PathBuf, executor: &BackgroundExecutor) -> FilePaint {
    let paint = executor
        .spawn({
            let path = path.clone();
            async move { load_file_preview(&path) }
        })
        .await;
    path_inflight().lock().unwrap().remove(&path);
    paint
}

/// 栅格文件还没有可用预览时，在后台解码，完成后刷新窗口。
/// 透明度为 0 的壁纸不要调用：那张图不该被读进来。
pub fn ensure_file_preview(path: &Path, cx: &App) {
    if !begin_file_fetch(path) {
        return;
    }
    let path = path.to_path_buf();
    cx.spawn(async move |cx| {
        let paint = fetch_file_preview(path, cx.background_executor()).await;
        cx.update(release_dropped_previews);
        if matches!(paint, FilePaint::Preview(_)) {
            cx.refresh();
        }
    })
    .detach();
}

/// 把文件画进调用方给的盒子。栅格图只用降采样预览；还没好或解不开时是空盒子。
pub fn file_surface(path: &Path, fit: FileFit) -> AnyElement {
    match file_paint(path) {
        FilePaint::Preview(render) => match fit {
            FileFit::Contain => contain_canvas(Some(render), 0.)
                .size_full()
                .into_any_element(),
            FileFit::Cover => cover_canvas(render).size_full().into_any_element(),
        },
        FilePaint::Placeholder | FilePaint::Failed => div().size_full().into_any_element(),
        FilePaint::Svg => {
            let image = img(path.to_path_buf()).size_full();
            match fit {
                FileFit::Contain => image.object_fit(ObjectFit::Contain).into_any_element(),
                FileFit::Cover => image.object_fit(ObjectFit::Cover).into_any_element(),
            }
        }
    }
}

/// 把内存里的图片完整放进调用方给的盒子（contain）。栅格图只画降采样预览，
/// 还没解好或解不开时是空盒子；调用方另行 [`ensure_preview`]。文件查看器用它：
/// 图片字节由运行时给，不在客户端按路径读盘。
pub fn image_surface(image: &Arc<gpui::Image>) -> AnyElement {
    match thumb_paint(image) {
        ThumbPaint::Preview(render) => contain_canvas(Some(render), 0.)
            .size_full()
            .into_any_element(),
        ThumbPaint::Placeholder => div().size_full().into_any_element(),
        ThumbPaint::Svg => img(Arc::clone(image))
            .size_full()
            .object_fit(ObjectFit::Contain)
            .into_any_element(),
    }
}

/// cover 时图片相对盒子左上角的位置和尺寸。多出来的部分由绘制时的盒子裁掉。
fn cover_placement(box_w: f32, box_h: f32, image_w: f32, image_h: f32) -> (f32, f32, f32, f32) {
    let scale = (box_w / image_w).max(box_h / image_h);
    let w = image_w * scale;
    let h = image_h * scale;
    ((box_w - w) / 2.0, (box_h - h) / 2.0, w, h)
}

/// contain 绘制：把 render 完整放入 bounds 并居中，不裁剪、不拉伸。
///
/// `contain_canvas` 内部用它，预览层等需要自定义 hitbox 的 canvas 也直接调它，
/// 保证所有场景的绘制路径完全一致。
pub fn paint_contain(
    bounds: Bounds<Pixels>,
    window: &mut Window,
    render: Option<&Arc<RenderImage>>,
    corner_radius: f32,
) {
    let Some(render) = render else {
        return;
    };
    let img_size = render.size(0);
    let (iw, ih) = (img_size.width.0 as f32, img_size.height.0 as f32);
    if iw <= 0.0 || ih <= 0.0 {
        return;
    }
    // contain：完整放入盒子并居中（可放大也可缩小）。
    let scale = (bounds.size.width / iw).min(bounds.size.height / ih);
    let w = iw * scale;
    let h = ih * scale;
    let origin = point(
        bounds.origin.x + (bounds.size.width - w) / 2.0,
        bounds.origin.y + (bounds.size.height - h) / 2.0,
    );
    let image_bounds = Bounds {
        origin,
        size: size(w, h),
    };
    let _ = window.paint_image(
        bounds,
        image_bounds,
        Corners::all(px(corner_radius)),
        render.clone(),
        0,
        false,
    );
}

fn paint_cover(bounds: Bounds<Pixels>, window: &mut Window, render: &Arc<RenderImage>) {
    let img_size = render.size(0);
    let (iw, ih) = (img_size.width.0 as f32, img_size.height.0 as f32);
    if iw <= 0.0 || ih <= 0.0 {
        return;
    }
    let (x, y, w, h) = cover_placement(
        bounds.size.width / px(1.),
        bounds.size.height / px(1.),
        iw,
        ih,
    );
    let image_bounds = Bounds {
        origin: point(bounds.origin.x + px(x), bounds.origin.y + px(y)),
        size: size(px(w), px(h)),
    };
    let _ = window.paint_image(
        bounds,
        image_bounds,
        Corners::all(px(0.)),
        Arc::clone(render),
        0,
        false,
    );
}

fn cover_canvas(render: Arc<RenderImage>) -> Canvas<()> {
    canvas(
        move |_bounds, _window, _cx| {},
        move |bounds, _prepaint, window, _cx| {
            paint_cover(bounds, window, &render);
        },
    )
}

/// contain 绘制 canvas：图片完整放入盒子并居中，不裁剪、不拉伸。
///
/// 盒子尺寸由调用点布局决定（`.h()`/`.w()`/`.size_full()` 等），绘制矩形在
/// paint 阶段按实际 bounds 自算，不依赖 taffy 的 aspect_ratio 推导——那正是
/// 历次「长截图高度被撑到几万像素、contain 失效只露顶部」的根因。`render` 为
/// None（解码未完成 / 失败）时画空白，调用点负责在解码完成后 notify 重绘。
pub fn contain_canvas(render: Option<Arc<RenderImage>>, corner_radius: f32) -> Canvas<()> {
    canvas(
        move |_bounds, _window, _cx| {},
        move |bounds, _prepaint, window, _cx| {
            paint_contain(bounds, window, render.as_ref(), corner_radius);
        },
    )
}

/// 固定高度的 contain Canvas。Canvas 没有 `img()` 的固有尺寸，单独设置高度和
/// `max_w` 会在收缩布局中得到零宽；这里根据已解码图片的比例显式给出宽度，同时
/// 保持宽度不超过调用方的上限。
fn contain_width_at_height(
    source_width: f32,
    source_height: f32,
    height: Pixels,
    max_width: Pixels,
) -> Pixels {
    if source_width > 0. && source_height > 0. {
        (height * (source_width / source_height)).min(max_width)
    } else {
        // 正常图片不会走到这里；仍保留非零布局盒，避免异常帧退化成不可见元素。
        max_width
    }
}

pub fn contain_canvas_at_height(
    render: Arc<RenderImage>,
    height: Pixels,
    max_width: Pixels,
    corner_radius: f32,
) -> Canvas<()> {
    let image_size = render.size(0);
    let width = contain_width_at_height(
        image_size.width.0 as f32,
        image_size.height.0 as f32,
        height,
        max_width,
    );
    contain_canvas(Some(render), corner_radius)
        .w(width)
        .h(height)
}

#[cfg(test)]
mod tests {
    // 注意：不能用 `use super::*`——image.rs 顶层的 `use gpui::*` 会把 gpui 的
    // `test` 属性宏带进测试模块，遮蔽标准 `#[test]`（gpui 宏会把函数体当测试
    // 上下文深度解析，rustc 直接 SIGBUS）。见 acp_view.rs 测试模块的同一惯例。
    use super::{
        CACHE_CAPACITY, FilePaint, ImageCacheInner, MAX_PREVIEW_DIM, StoredPreview, ThumbPaint,
        begin_file_fetch, begin_preview, contain_width_at_height, cover_placement, decode_preview,
        file_paint, finish_preview, load_file_preview, preview_pending, put, store_preview,
        thumb_paint,
    };
    use std::collections::{HashMap, HashSet, VecDeque};
    use std::sync::Arc;

    fn png_bytes(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([255u8, 0, 0, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    fn make_image(bytes: Vec<u8>) -> Arc<gpui::Image> {
        Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, bytes))
    }

    #[test]
    fn thumbnail_width_uses_source_aspect_ratio_and_cap() {
        assert_eq!(
            contain_width_at_height(400., 200., gpui::px(160.), gpui::px(280.)),
            gpui::px(280.)
        );
        assert_eq!(
            contain_width_at_height(200., 400., gpui::px(160.), gpui::px(280.)),
            gpui::px(80.)
        );
        assert_eq!(
            contain_width_at_height(0., 200., gpui::px(160.), gpui::px(280.)),
            gpui::px(280.)
        );
    }

    #[test]
    fn small_image_is_not_resized() {
        let render = decode_preview(&make_image(png_bytes(64, 48))).expect("png 应可解码");
        let size = render.size(0);
        assert_eq!(size.width.0 as u32, 64);
        assert_eq!(size.height.0 as u32, 48);
    }

    #[test]
    fn uncached_raster_thumb_does_not_paint_the_original() {
        // 尺寸必须和会 `put` 进进程全局缓存的用例不同，否则执行顺序会让这条断言碰到预览。
        let image = make_image(png_bytes(32, 7000));
        assert!(
            matches!(thumb_paint(&image), ThumbPaint::Placeholder),
            "预览未完成时不能按原图像素绘制"
        );
    }

    #[test]
    fn cached_raster_thumb_is_the_downsampled_preview() {
        let image = make_image(png_bytes(64, 6000));
        let render = decode_preview(&image).expect("png 应可解码");
        put(image.id, render);
        let ThumbPaint::Preview(render) = thumb_paint(&image) else {
            panic!("入缓存后应画降采样预览");
        };
        let size = render.size(0);
        assert!((size.width.0 as u32) <= MAX_PREVIEW_DIM);
        assert!((size.height.0 as u32) <= MAX_PREVIEW_DIM);
    }

    #[test]
    fn svg_thumb_stays_on_the_vector_path() {
        let image = gpui::Image::from_bytes(gpui::ImageFormat::Svg, b"<svg/>".to_vec());
        assert!(matches!(thumb_paint(&image), ThumbPaint::Svg));
    }

    #[test]
    fn huge_image_is_downsampled() {
        // 超高长截图：高度被压到上限以内，宽度等比缩小。image crate 的 resize
        // 是分步缩小（每步最多减半，保证缩放质量），最终尺寸不精确等于目标，
        // 断言只验证「不超上限 + 确实缩小」。
        let render = decode_preview(&make_image(png_bytes(64, 6000))).expect("png 应可解码");
        let size = render.size(0);
        assert!((size.height.0 as u32) <= MAX_PREVIEW_DIM);
        assert!((size.height.0 as u32) > 1000); // 从 6000 压下来了
        assert!((size.width.0 as u32) < 64);
    }

    #[test]
    fn invalid_bytes_fail_silently() {
        assert!(decode_preview(&make_image(b"not an image".to_vec())).is_none());
    }

    #[test]
    fn uncached_raster_file_does_not_paint_the_original() {
        let path = write_png("smelt-file-preview-uncached.png", 24, 5000);
        assert!(
            matches!(file_paint(&path), FilePaint::Placeholder),
            "文件预览未完成时不能按原图像素绘制"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn cached_raster_file_is_the_downsampled_preview() {
        let path = write_png("smelt-file-preview-cached.png", 40, 5000);
        let FilePaint::Preview(render) = load_file_preview(&path) else {
            panic!("可读的栅格文件应留下降采样预览");
        };
        let size = render.size(0);
        assert!((size.width.0 as u32) <= MAX_PREVIEW_DIM);
        assert!((size.height.0 as u32) <= MAX_PREVIEW_DIM);
        assert!((size.height.0 as u32) < 5000);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn svg_file_stays_on_the_vector_path() {
        let path = std::path::Path::new("/tmp/smelt-file-preview.svg");
        assert!(matches!(file_paint(path), FilePaint::Svg));
        assert!(!begin_file_fetch(path));
    }

    #[test]
    fn failed_raster_file_is_not_fetched_again() {
        let path = std::env::temp_dir().join("smelt-file-preview-bad.png");
        std::fs::write(&path, b"not an image").unwrap();
        assert!(matches!(load_file_preview(&path), FilePaint::Failed));
        assert!(
            !begin_file_fetch(&path),
            "同一份解不开的文件不能每帧再读一遍"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn cover_crops_the_long_edge() {
        let (x, y, w, h) = cover_placement(100., 100., 200., 50.);
        assert!((w - 400.).abs() < 0.01);
        assert!((h - 100.).abs() < 0.01);
        assert!((x - -150.).abs() < 0.01);
        assert!(y.abs() < 0.01);
    }

    fn write_png(name: &str, w: u32, h: u32) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(name);
        std::fs::write(&path, png_bytes(w, h)).unwrap();
        path
    }

    #[test]
    fn image_id_is_byte_addressed() {
        assert_eq!(
            make_image(png_bytes(8, 8)).id,
            make_image(png_bytes(8, 8)).id
        );
        assert_ne!(
            make_image(png_bytes(8, 8)).id,
            make_image(png_bytes(9, 8)).id
        );
    }

    #[test]
    fn evicting_a_preview_keeps_the_render_image_for_atlas_release() {
        let mut cache = ImageCacheInner {
            entries: HashMap::new(),
            order: VecDeque::new(),
            inflight: HashSet::new(),
            pending_drops: Vec::new(),
        };
        let mut first_key = None;
        let mut first_render_id = None;
        for index in 0..(CACHE_CAPACITY + 1) {
            let image = make_image(png_bytes(2 + index as u32, 2));
            let render = decode_preview(&image).expect("小图应可解码");
            if index == 0 {
                first_key = Some(image.id);
                first_render_id = Some(render.id);
            }
            store_preview(&mut cache, image.id, StoredPreview::Ready(render));
        }
        assert!(
            !cache.entries.contains_key(&first_key.expect("first")),
            "超出上限后最旧的预览必须离开 CPU 缓存"
        );
        assert_eq!(cache.entries.len(), CACHE_CAPACITY);
        assert!(
            cache
                .pending_drops
                .iter()
                .any(|render| render.id == first_render_id.expect("render id")),
            "被淘汰的预览还要交给 drop_image，不能只丢掉 Arc"
        );
    }

    #[test]
    fn evicting_a_failed_preview_does_not_request_atlas_release() {
        let mut cache = ImageCacheInner {
            entries: HashMap::new(),
            order: VecDeque::new(),
            inflight: HashSet::new(),
            pending_drops: Vec::new(),
        };
        store_preview(&mut cache, u64::MAX, StoredPreview::Failed);
        for index in 0..CACHE_CAPACITY {
            let image = make_image(png_bytes(80 + index as u32, 3));
            let render = decode_preview(&image).expect("小图应可解码");
            store_preview(&mut cache, image.id, StoredPreview::Ready(render));
        }
        assert!(cache.pending_drops.is_empty(), "失败记录没有上传 atlas");
        assert!(!cache.entries.contains_key(&u64::MAX));
    }

    #[test]
    fn preview_starts_once_and_a_failure_is_not_retried() {
        let image = make_image(png_bytes(17, 19));
        assert!(preview_pending(&image));
        assert!(begin_preview(&image));
        assert!(!begin_preview(&image), "在途的解码不能再启动一次");
        finish_preview(image.id, None);
        assert!(!preview_pending(&image), "失败要留下，绘制路径不能每帧再试");
        assert!(!begin_preview(&image));
    }
}
