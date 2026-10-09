//! 手机快照里的图片。
//!
//! 截图类工具结果一张就是 1～2MB base64，一个会话十几张就是十几 MB。快照走同
//! 一条 WebSocket，大帧传输期间心跳 pong 排在后面，经 relay 时超过手机的心跳
//! 超时，手机断线重连再要一份全量快照，永远加载不出来。
//!
//! 所以快照里大图只带缩略图，加一个 `original` 引用（顶层条目下标 + 原图摘要）。
//! 手机点开大图时用 `loadImage` 按引用取一份限过尺寸的原图。

use std::collections::{HashMap, VecDeque};
use std::io::Cursor;
use std::sync::{Arc, Mutex, OnceLock};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use image::{DynamicImage, GenericImageView as _, ImageFormat};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

/// 这么小的图原样发，缩略反而可能更大。
const INLINE_MAX_B64: usize = 64 * 1024;
/// 列表里的缩略图是 112×88 逻辑像素、cover 裁切；3 倍屏的短边要 264 像素。
const THUMB_SHORT_SIDE: u32 = 320;
/// 极长的图只留中间这一段做缩略图，cover 裁切本来也看不到两头。
const THUMB_LONG_SIDE: u32 = 960;
const THUMB_JPEG_QUALITY: u8 = 75;
/// 点开看的大图：超过这个大小就压到手机屏幕够用的尺寸，免得一张图又把心跳堵住。
const VIEW_MAX_B64: usize = 1024 * 1024;
const VIEW_LONG_SIDE: u32 = 2560;
const VIEW_JPEG_QUALITY: u8 = 85;
const THUMB_CACHE_CAPACITY: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EncodedImage {
    pub mime: String,
    pub data_b64: String,
}

/// 把一行 daemon 快照改成手机要的样子：带上会话 id，大图换缩略图，去掉手机不
/// 用的大字段。解析不了就原样转发。
pub(crate) fn mobile_snapshot_line(line: &str, session_id: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<Value>(line) else {
        return line.to_string();
    };
    if let Some(snapshot) = value.get_mut("snapshot") {
        shrink_snapshot_images(snapshot);
    }
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "sessionId".to_string(),
            Value::String(session_id.to_string()),
        );
    }
    value.to_string()
}

fn shrink_snapshot_images(snapshot: &mut Value) {
    let Some(snapshot) = snapshot.as_object_mut() else {
        return;
    };
    // 被撤回到输入框的附图只有桌面输入框会用。
    snapshot.remove("composer_restore_images");
    let offset = snapshot
        .get("entries_offset")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let Some(entries) = snapshot.get_mut("entries").and_then(Value::as_array_mut) else {
        return;
    };
    for (index, entry) in entries.iter_mut().enumerate() {
        let entry_index = offset + index as u64;
        for_each_image_mut(entry, &mut |image| shrink_image(image, entry_index));
    }
}

/// 一条消息里所有图片：用户附图、工具输出图，以及子代理嵌套条目里的。
fn for_each_image_mut(entry: &mut Value, f: &mut impl FnMut(&mut Value)) {
    if let Some(images) = entry
        .pointer_mut("/UserWithImages/images")
        .and_then(Value::as_array_mut)
    {
        images.iter_mut().for_each(&mut *f);
    }
    let Some(tool) = entry.get_mut("ToolCall") else {
        return;
    };
    if let Some(output) = tool.get_mut("output").and_then(Value::as_array_mut) {
        for part in output {
            if let Some(image) = part.get_mut("Image") {
                f(image);
            }
        }
    }
    if let Some(children) = tool.get_mut("children").and_then(Value::as_array_mut) {
        for child in children {
            for_each_image_mut(child, f);
        }
    }
}

fn shrink_image(image: &mut Value, entry_index: u64) {
    let Some(data) = image.get("data_b64").and_then(Value::as_str) else {
        return;
    };
    if data.len() <= INLINE_MAX_B64 {
        return;
    }
    let digest = digest(data);
    // 解不开的格式也不能把原图塞进快照：留空，手机画占位图，点开再取。
    let thumb = cached_thumbnail(&digest, data).unwrap_or_else(|| EncodedImage {
        mime: image
            .get("mime")
            .and_then(Value::as_str)
            .unwrap_or("image/png")
            .to_string(),
        data_b64: String::new(),
    });
    *image = serde_json::json!({
        "mime": thumb.mime,
        "data_b64": thumb.data_b64,
        "original": {"entry": entry_index, "digest": digest},
    });
}

/// 按 `loadImage` 的引用在一条 daemon 快照里找原图，压到手机看图够用的大小。
/// 条目被重写过（回退、压缩）时摘要对不上，返回 None。
pub(crate) fn original_image(
    snapshot_line: &str,
    entry_index: u64,
    digest_hex: &str,
) -> Option<EncodedImage> {
    let value: Value = serde_json::from_str(snapshot_line).ok()?;
    let snapshot = value.get("snapshot")?;
    let offset = snapshot.get("entries_offset")?.as_u64()?;
    let entries = snapshot.get("entries")?.as_array()?;
    let mut entry = entries
        .get(entry_index.checked_sub(offset)? as usize)?
        .clone();
    let mut found = None;
    for_each_image_mut(&mut entry, &mut |image| {
        if found.is_some() {
            return;
        }
        let Some(data) = image.get("data_b64").and_then(Value::as_str) else {
            return;
        };
        if digest(data) == digest_hex {
            found = Some(EncodedImage {
                mime: image
                    .get("mime")
                    .and_then(Value::as_str)
                    .unwrap_or("image/png")
                    .to_string(),
                data_b64: data.to_string(),
            });
        }
    });
    found.map(viewable)
}

fn viewable(image: EncodedImage) -> EncodedImage {
    if image.data_b64.len() <= VIEW_MAX_B64 {
        return image;
    }
    let Some(decoded) = decode(&image.data_b64) else {
        return image;
    };
    let (width, height) = decoded.dimensions();
    let resized = if width.max(height) > VIEW_LONG_SIDE {
        decoded.resize(
            VIEW_LONG_SIDE,
            VIEW_LONG_SIDE,
            image::imageops::FilterType::Triangle,
        )
    } else {
        decoded
    };
    encode(&resized, VIEW_JPEG_QUALITY).unwrap_or(image)
}

fn digest(data_b64: &str) -> String {
    let hash = Sha256::digest(data_b64.as_bytes());
    hash[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn cached_thumbnail(digest: &str, data_b64: &str) -> Option<EncodedImage> {
    type Cache = (HashMap<String, Arc<EncodedImage>>, VecDeque<String>);
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(hit) = cache.lock().unwrap().0.get(digest) {
        return Some(EncodedImage::clone(hit));
    }
    let thumb = thumbnail(data_b64)?;
    let mut guard = cache.lock().unwrap();
    let (map, order) = &mut *guard;
    if map
        .insert(digest.to_string(), Arc::new(thumb.clone()))
        .is_none()
    {
        order.push_back(digest.to_string());
    }
    while order.len() > THUMB_CACHE_CAPACITY {
        if let Some(oldest) = order.pop_front() {
            map.remove(&oldest);
        }
    }
    Some(thumb)
}

fn thumbnail(data_b64: &str) -> Option<EncodedImage> {
    let decoded = decode(data_b64)?;
    let (width, height) = decoded.dimensions();
    let short = width.min(height).max(1);
    let scaled = if short > THUMB_SHORT_SIDE {
        let ratio = THUMB_SHORT_SIDE as f64 / short as f64;
        let w = ((width as f64 * ratio).round() as u32).max(1);
        let h = ((height as f64 * ratio).round() as u32).max(1);
        decoded.thumbnail_exact(w, h)
    } else {
        decoded
    };
    let (width, height) = scaled.dimensions();
    let cropped = if width > THUMB_LONG_SIDE {
        scaled.crop_imm((width - THUMB_LONG_SIDE) / 2, 0, THUMB_LONG_SIDE, height)
    } else if height > THUMB_LONG_SIDE {
        scaled.crop_imm(0, (height - THUMB_LONG_SIDE) / 2, width, THUMB_LONG_SIDE)
    } else {
        scaled
    };
    encode(&cropped, THUMB_JPEG_QUALITY)
}

fn decode(data_b64: &str) -> Option<DynamicImage> {
    let bytes = BASE64.decode(data_b64).ok()?;
    image::load_from_memory(&bytes).ok()
}

/// 不透明图出 JPEG；真用到透明的才出 PNG，不然透明处会变成黑底。
fn encode(image: &DynamicImage, jpeg_quality: u8) -> Option<EncodedImage> {
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
    Some(EncodedImage {
        mime: mime.to_string(),
        data_b64: BASE64.encode(bytes),
    })
}

fn uses_alpha(image: &DynamicImage) -> bool {
    image.color().has_alpha() && image.to_rgba8().pixels().any(|pixel| pixel[3] < u8::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_b64(width: u32, height: u32, alpha: u8) -> String {
        // 渐变叠低幅噪点：PNG 压不动（像真实截图一样大），JPEG 能压。
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

    fn snapshot_line(offset: u64, entries: Value) -> String {
        serde_json::json!({
            "snapshot": {
                "entries_offset": offset,
                "entries": entries,
                "composer_restore_images": [{"mime": "image/png", "data_b64": "AAAA"}],
            }
        })
        .to_string()
    }

    #[test]
    fn large_images_become_thumbnails_with_a_reference_to_the_original() {
        let screenshot = png_b64(1170, 2532, u8::MAX);
        assert!(screenshot.len() > 1024 * 1024);
        let line = snapshot_line(
            40,
            serde_json::json!([
                {"User": "hi"},
                {"ToolCall": {
                    "id": "t1", "title": "read", "kind": "read", "status": "completed",
                    "output": [
                        {"Text": "Read image file"},
                        {"Image": {"mime": "image/png", "data_b64": screenshot}},
                    ],
                    "children": [
                        {"UserWithImages": {"text": "nested", "images": [
                            {"mime": "image/png", "data_b64": screenshot},
                        ]}},
                    ],
                }},
                {"UserWithImages": {"text": "tiny", "images": [
                    {"mime": "image/png", "data_b64": "iVBORw0KGgo="},
                ]}},
            ]),
        );

        let out: Value = serde_json::from_str(&mobile_snapshot_line(&line, "acp-1")).unwrap();
        assert_eq!(out["sessionId"], "acp-1");
        assert!(out["snapshot"].get("composer_restore_images").is_none());
        let image = &out["snapshot"]["entries"][1]["ToolCall"]["output"][1]["Image"];
        assert_eq!(image["mime"], "image/jpeg");
        assert!(image["data_b64"].as_str().unwrap().len() < 128 * 1024);
        assert_eq!(image["original"]["entry"], 41);
        assert_eq!(image["original"]["digest"], digest(&screenshot));
        let thumb = decode(image["data_b64"].as_str().unwrap()).unwrap();
        assert_eq!(thumb.dimensions(), (320, 693));
        let nested = &out["snapshot"]["entries"][1]["ToolCall"]["children"][0]["UserWithImages"]["images"]
            [0];
        assert_eq!(nested["original"]["entry"], 41);
        let tiny = &out["snapshot"]["entries"][2]["UserWithImages"]["images"][0];
        assert_eq!(tiny["data_b64"], "iVBORw0KGgo=");
        assert!(tiny.get("original").is_none());

        let original = original_image(&line, 41, &digest(&screenshot)).unwrap();
        assert_eq!(original.mime, "image/jpeg");
        assert!(original.data_b64.len() <= VIEW_MAX_B64);
        assert_eq!(
            decode(&original.data_b64).unwrap().dimensions(),
            (1170, 2532)
        );
        assert!(original_image(&line, 41, "0000").is_none());
        assert!(original_image(&line, 39, &digest(&screenshot)).is_none());
    }

    #[test]
    fn transparent_images_keep_their_alpha_and_undecodable_ones_are_not_inlined() {
        let transparent = png_b64(400, 300, 128);
        let garbage = BASE64.encode(vec![7u8; 100 * 1024]);
        let line = snapshot_line(
            0,
            serde_json::json!([{"UserWithImages": {"text": "", "images": [
                {"mime": "image/png", "data_b64": transparent},
                {"mime": "image/heic", "data_b64": garbage},
            ]}}]),
        );
        let out: Value = serde_json::from_str(&mobile_snapshot_line(&line, "acp-1")).unwrap();
        let images = &out["snapshot"]["entries"][0]["UserWithImages"]["images"];
        assert_eq!(images[0]["mime"], "image/png");
        assert_eq!(
            decode(images[0]["data_b64"].as_str().unwrap())
                .unwrap()
                .dimensions(),
            (400, 300)
        );
        assert_eq!(images[1]["mime"], "image/heic");
        assert_eq!(images[1]["data_b64"], "");
        assert_eq!(images[1]["original"]["entry"], 0);
    }

    #[test]
    fn small_originals_are_sent_as_is() {
        let small = png_b64(300, 200, u8::MAX);
        assert!(small.len() > INLINE_MAX_B64 && small.len() < VIEW_MAX_B64);
        let line = snapshot_line(
            0,
            serde_json::json!([{"UserWithImages": {"text": "", "images": [
                {"mime": "image/png", "data_b64": small},
            ]}}]),
        );
        let original = original_image(&line, 0, &digest(&small)).unwrap();
        assert_eq!(
            original,
            EncodedImage {
                mime: "image/png".into(),
                data_b64: small
            }
        );
    }
}
