//! `conversation.image`：按引用取对话里一张没有内联的图。
//!
//! 要图片引用的连接（`acp_watch` / `acp_snapshot` 带 `image_refs`）上，大图只发
//! `{entry, digest}`，出帧时不碰图片内容。客户端显示时来这里取：带 `max_edge` 出缩略图，
//! 不带出可看的原图。条目和 `acp_snapshot` 翻页走同一条路找（挂起的会话先唤醒再读）；
//! 解码、缩放、编码都在拿到条目副本之后做，不占会话的锁。
//! 结果按 (摘要, 尺寸) 缓存。

use std::collections::VecDeque;
use std::io::Cursor;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use image::{DynamicImage, GenericImageView as _, ImageFormat};
use smelt_core::control_api::{ConversationImageParams, ConversationImageResult};
use smelt_runtime_client::protocol::control::{ControlError, ControlErrorCode};

use super::{AcpSessions, EventHubHandle};
use crate::acp_host::{HistoryReadError, read_history_page};

/// 缩略图长边的合理范围：再小看不清，再大就该取原图了。
const THUMB_EDGE_MIN: u32 = 32;
const THUMB_EDGE_MAX: u32 = 2048;
const THUMB_JPEG_QUALITY: u8 = 75;
/// 点开看的大图：这么大以内原样给，超过就压到手机屏幕够用的尺寸，免得一张图堵住连接。
const VIEW_MAX_B64: usize = 1024 * 1024;
const VIEW_LONG_EDGE: u32 = 2560;
const VIEW_JPEG_QUALITY: u8 = 85;
/// 缓存只为手机来回滚动时同一批缩略图不重复解码：条数和总字节都封顶。
const CACHE_MAX_ENTRIES: usize = 64;
const CACHE_MAX_BYTES: usize = 32 * 1024 * 1024;

pub(crate) fn conversation_image(
    params: ConversationImageParams,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<ConversationImageResult, ControlError> {
    let ConversationImageParams {
        session_id,
        entry,
        digest,
        max_edge,
    } = params;
    let original = find_image(&session_id, entry, &digest, acp_sessions, subscribers)?;
    let max_edge = max_edge.map(|edge| edge.clamp(THUMB_EDGE_MIN, THUMB_EDGE_MAX));
    render(original, &digest, max_edge)
}

fn image_gone() -> ControlError {
    ControlError::new(
        ControlErrorCode::NotFound,
        "这张图已不在对话里（条目被回退或压缩改写过）",
    )
}

/// 在第 `entry` 条（全局下标）的图片里找摘要对得上的那张。
fn find_image(
    session_id: &str,
    entry: u64,
    digest: &str,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<ConversationImageResult, ControlError> {
    let index = usize::try_from(entry).map_err(|_| image_gone())?;
    let page = read_history_page(
        session_id,
        acp_sessions,
        subscribers,
        index.saturating_add(1),
        1,
        false,
    )
    .map_err(|error| match error {
        HistoryReadError::SessionMissing => {
            ControlError::new(ControlErrorCode::NotFound, "对话不存在或已结束")
        }
        HistoryReadError::Failed(error) => {
            ControlError::new(ControlErrorCode::OperationFailed, error)
        }
    })?;
    // 下标超出末尾时这一页是最后一条，不是要的那条。
    let entry = index
        .checked_sub(page.entries_offset)
        .and_then(|local| page.entries.get(local))
        .ok_or_else(image_gone)?;
    let mut found = None;
    smelt_core::acp_chat::for_each_image(entry, &mut |image| {
        if found.is_none() && !image.data_b64.is_empty() && image.digest() == digest {
            found = Some(ConversationImageResult {
                mime: image.mime.clone(),
                data_b64: image.data_b64.clone(),
            });
        }
    });
    found.ok_or_else(image_gone)
}

/// 按请求的尺寸出图。`max_edge` 已夹到合理范围。
fn render(
    original: ConversationImageResult,
    digest: &str,
    max_edge: Option<u32>,
) -> Result<ConversationImageResult, ControlError> {
    if max_edge.is_none() && original.data_b64.len() <= VIEW_MAX_B64 {
        return Ok(original);
    }
    let key = (digest.to_string(), max_edge);
    if let Some(hit) = cache_get(&key) {
        return Ok(ConversationImageResult::clone(&hit));
    }
    let rendered = match max_edge {
        Some(edge) => thumbnail(&original.data_b64, edge).ok_or_else(|| {
            ControlError::new(
                ControlErrorCode::OperationFailed,
                format!("{} 解不开，出不了缩略图", original.mime),
            )
        })?,
        // 解不开的格式（比如 HEIC）原样给，客户端也许自己会解。
        None => match viewable(&original.data_b64) {
            Some(rendered) => rendered,
            None => return Ok(original),
        },
    };
    cache_put(key, &rendered);
    Ok(rendered)
}

fn thumbnail(data_b64: &str, max_edge: u32) -> Option<ConversationImageResult> {
    let decoded = decode(data_b64)?;
    let (width, height) = decoded.dimensions();
    let fitted = if width.max(height) > max_edge {
        // 缩略图用快速的整数缩放，够看。
        decoded.thumbnail(max_edge, max_edge)
    } else {
        decoded
    };
    encode(&fitted, THUMB_JPEG_QUALITY)
}

fn viewable(data_b64: &str) -> Option<ConversationImageResult> {
    let decoded = decode(data_b64)?;
    let (width, height) = decoded.dimensions();
    let fitted = if width.max(height) > VIEW_LONG_EDGE {
        decoded.resize(
            VIEW_LONG_EDGE,
            VIEW_LONG_EDGE,
            image::imageops::FilterType::Triangle,
        )
    } else {
        decoded
    };
    encode(&fitted, VIEW_JPEG_QUALITY)
}

fn decode(data_b64: &str) -> Option<DynamicImage> {
    let bytes = BASE64.decode(data_b64).ok()?;
    image::load_from_memory(&bytes).ok()
}

/// 不透明图出 JPEG；真用到透明的才出 PNG，不然透明处会变成黑底。
fn encode(image: &DynamicImage, jpeg_quality: u8) -> Option<ConversationImageResult> {
    let mut bytes = Vec::new();
    let mime = if uses_alpha(image) {
        image
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .ok()?;
        "image/png"
    } else {
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, jpeg_quality)
            .encode_image(&image.to_rgb8())
            .ok()?;
        "image/jpeg"
    };
    Some(ConversationImageResult {
        mime: mime.to_string(),
        data_b64: BASE64.encode(bytes),
    })
}

fn uses_alpha(image: &DynamicImage) -> bool {
    match image {
        DynamicImage::ImageRgba8(buffer) => buffer.pixels().any(|pixel| pixel[3] < u8::MAX),
        _ if image.color().has_alpha() => image.to_rgba8().pixels().any(|pixel| pixel[3] < u8::MAX),
        _ => false,
    }
}

type CacheKey = (String, Option<u32>);

/// 最近用过的在队尾。
struct RenderCache {
    entries: VecDeque<(CacheKey, Arc<ConversationImageResult>)>,
    bytes: usize,
}

static CACHE: Mutex<RenderCache> = Mutex::new(RenderCache {
    entries: VecDeque::new(),
    bytes: 0,
});

fn cache_get(key: &CacheKey) -> Option<Arc<ConversationImageResult>> {
    let mut cache = CACHE.lock().unwrap();
    let position = cache.entries.iter().position(|(cached, _)| cached == key)?;
    let hit = cache.entries.remove(position)?;
    let image = Arc::clone(&hit.1);
    cache.entries.push_back(hit);
    Some(image)
}

fn cache_put(key: CacheKey, image: &ConversationImageResult) {
    let size = image.data_b64.len();
    if size > CACHE_MAX_BYTES {
        return;
    }
    let mut cache = CACHE.lock().unwrap();
    if let Some(position) = cache.entries.iter().position(|(cached, _)| *cached == key)
        && let Some((_, old)) = cache.entries.remove(position)
    {
        cache.bytes -= old.data_b64.len();
    }
    cache.entries.push_back((key, Arc::new(image.clone())));
    cache.bytes += size;
    while cache.entries.len() > CACHE_MAX_ENTRIES || cache.bytes > CACHE_MAX_BYTES {
        let Some((_, oldest)) = cache.entries.pop_front() else {
            break;
        };
        cache.bytes -= oldest.data_b64.len();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// 渐变叠低幅噪点：PNG 压不动（像真实截图一样大），JPEG 能压。
    pub(crate) fn png_b64(width: u32, height: u32, alpha: u8) -> String {
        let image = image::RgbaImage::from_fn(width, height, |x, y| {
            let n = x
                .wrapping_mul(2_654_435_761)
                .wrapping_add(y.wrapping_mul(40_503));
            let noise = |shift: u32| ((n >> shift) & 15) as u8;
            image::Rgba([
                (x % 240) as u8 + noise(0),
                (y % 240) as u8 + noise(4),
                ((x + y) % 240) as u8 + noise(8),
                alpha,
            ])
        });
        let mut bytes = Vec::new();
        DynamicImage::ImageRgba8(image)
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();
        BASE64.encode(bytes)
    }

    fn png(data_b64: String) -> ConversationImageResult {
        ConversationImageResult {
            mime: "image/png".into(),
            data_b64,
        }
    }

    pub(crate) fn dimensions(data_b64: &str) -> (u32, u32) {
        decode(data_b64).unwrap().dimensions()
    }

    #[test]
    fn thumbnails_fit_the_long_edge_without_cropping() {
        let screenshot = png_b64(1170, 2532, u8::MAX);
        let digest = smelt_core::acp_chat::image_digest(&screenshot);
        let thumb = render(png(screenshot.clone()), &digest, Some(320)).unwrap();
        assert_eq!(thumb.mime, "image/jpeg");
        assert_eq!(dimensions(&thumb.data_b64), (148, 320));
        assert!(cache_get(&(digest.clone(), Some(320))).is_some());
        // 第二次走缓存，结果一样。
        assert_eq!(render(png(screenshot), &digest, Some(320)).unwrap(), thumb);

        // 比要的还小就不放大，只重新编码。
        let small = png_b64(100, 60, u8::MAX);
        let digest = smelt_core::acp_chat::image_digest(&small);
        let thumb = render(png(small), &digest, Some(320)).unwrap();
        assert_eq!(dimensions(&thumb.data_b64), (100, 60));
    }

    #[test]
    fn transparent_images_keep_their_alpha() {
        let transparent = png_b64(400, 300, 128);
        let digest = smelt_core::acp_chat::image_digest(&transparent);
        let thumb = render(png(transparent), &digest, Some(200)).unwrap();
        assert_eq!(thumb.mime, "image/png");
        assert_eq!(dimensions(&thumb.data_b64), (200, 150));
    }

    #[test]
    fn originals_are_sent_as_is_up_to_a_megabyte_and_shrunk_beyond() {
        let small = png_b64(300, 200, u8::MAX);
        assert!(small.len() > 64 * 1024 && small.len() <= VIEW_MAX_B64);
        let digest = smelt_core::acp_chat::image_digest(&small);
        assert_eq!(
            render(png(small.clone()), &digest, None).unwrap(),
            png(small)
        );

        let wide = png_b64(2700, 1000, u8::MAX);
        assert!(wide.len() > VIEW_MAX_B64);
        let digest = smelt_core::acp_chat::image_digest(&wide);
        let view = render(png(wide), &digest, None).unwrap();
        assert_eq!(view.mime, "image/jpeg");
        assert!(view.data_b64.len() <= VIEW_MAX_B64);
        assert_eq!(dimensions(&view.data_b64), (2560, 948));
    }

    #[test]
    fn undecodable_images_fail_thumbnails_but_still_open() {
        let garbage = BASE64.encode(vec![7u8; 2 * VIEW_MAX_B64]);
        let digest = smelt_core::acp_chat::image_digest(&garbage);
        let heic = ConversationImageResult {
            mime: "image/heic".into(),
            data_b64: garbage,
        };
        let error = render(heic.clone(), &digest, Some(320)).unwrap_err();
        assert_eq!(error.code, ControlErrorCode::OperationFailed);
        assert_eq!(render(heic.clone(), &digest, None).unwrap(), heic);
    }
}
