//! 会话图片字节与缩略图的有界内存缓存（纯非 UI，零 GPUI）。
//!
//! - 粘贴图片、拖拽本地图片或快照里超过 [`COMPACT_IMAGE_THRESHOLD_B64`] 的内联图片，
//!   统一落到这里的内存表（按内容 SHA-256 去重，FIFO 封顶 [`MAX_STORED_IMAGES`]）；
//! - 给 QuickJS 只传轻量 `ref_id` + 缩略图 `preview_uri`（最大边 256px），大 base64 不进 JS 堆；
//! - 提交消息时 JS 只传 `image_refs: Vec<String>`，由本层还原为 `AcpImage`（`PromptImage`）写给 `smeltd`；
//! - 用户点开查看大图时按 `ref_id` 按需取原图 `data:<mime>;base64,...`。

use std::collections::{HashMap, VecDeque};
use std::io::Cursor;
use std::path::Path;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use image::ImageFormat;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use smelt_plugin_api::model::acp_chat::AcpImage;

/// 单张图片字节上限（20 MiB）。
pub const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;
/// 进程内最多保留的图片张数（超出按最旧插入淘汰）。
pub const MAX_STORED_IMAGES: usize = 256;
/// 快照里 base64 长度超过此阈值（48 KiB）的图片压缩成引用 + 缩略图再推给 JS。
pub const COMPACT_IMAGE_THRESHOLD_B64: usize = 48 * 1024;
/// 低于此字节数（32 KiB）的图片直接用原图当缩略图，免一次重编码。
const DIRECT_PREVIEW_MAX_BYTES: usize = 32 * 1024;
/// 缩略图最大边长（像素）。
const THUMBNAIL_MAX_SIDE: u32 = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredImageMeta {
    pub ref_id: String,
    pub mime: String,
    pub width: u32,
    pub height: u32,
    pub byte_len: usize,
    pub preview_uri: String,
}

struct StoredImage {
    meta: StoredImageMeta,
    data_b64: Arc<str>,
}

#[derive(Default)]
struct Inner {
    by_ref: HashMap<String, StoredImage>,
    /// 以原 base64 的 SHA-256 为键做快速去重，避免每帧快照对同一张大图反复解码 base64。
    by_b64_hash: HashMap<[u8; 32], String>,
    order: VecDeque<(String, Option<[u8; 32]>)>,
}

impl Inner {
    fn insert(&mut self, entry: StoredImage, b64_hash: Option<[u8; 32]>) -> StoredImageMeta {
        let ref_id = entry.meta.ref_id.clone();
        let meta = entry.meta.clone();
        if self.by_ref.contains_key(&ref_id) {
            if let Some(hash) = b64_hash {
                self.by_b64_hash.insert(hash, ref_id);
            }
            return self.by_ref[&meta.ref_id].meta.clone();
        }
        while self.by_ref.len() >= MAX_STORED_IMAGES {
            if let Some((evicted_id, evicted_hash)) = self.order.pop_front() {
                self.by_ref.remove(&evicted_id);
                if let Some(hash) = evicted_hash {
                    self.by_b64_hash.remove(&hash);
                }
            } else {
                break;
            }
        }
        if let Some(hash) = b64_hash {
            self.by_b64_hash.insert(hash, ref_id.clone());
        }
        self.by_ref.insert(ref_id.clone(), entry);
        self.order.push_back((ref_id, b64_hash));
        meta
    }
}

#[derive(Clone, Default)]
pub struct ImageStore(Arc<Mutex<Inner>>);

impl ImageStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 存入原始图片字节，返回元数据与轻量缩略图 data URI。
    pub fn store_bytes(
        &self,
        bytes: Vec<u8>,
        mime_hint: Option<&str>,
    ) -> Result<StoredImageMeta, String> {
        if bytes.is_empty() {
            return Err("image is empty".into());
        }
        if bytes.len() > MAX_IMAGE_BYTES {
            return Err(format!(
                "image exceeds limit ({} > {} bytes)",
                bytes.len(),
                MAX_IMAGE_BYTES
            ));
        }

        let digest = sha256_hex_16(&bytes);
        {
            let guard = self.0.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(existing) = guard.by_ref.get(&digest) {
                return Ok(existing.meta.clone());
            }
        }

        let mime = sniff_mime(&bytes, mime_hint)?;
        let (width, height, preview_uri) = build_preview(&bytes, &mime)?;
        let data_b64 = BASE64.encode(&bytes);
        let b64_hash = sha256_bytes(data_b64.as_bytes());
        let meta = StoredImageMeta {
            ref_id: digest,
            mime,
            width,
            height,
            byte_len: bytes.len(),
            preview_uri,
        };
        let stored = StoredImage {
            meta,
            data_b64: Arc::from(data_b64),
        };

        let mut guard = self.0.lock().unwrap_or_else(|e| e.into_inner());
        Ok(guard.insert(stored, Some(b64_hash)))
    }

    /// 从本地文件读取图片存入缓存（阻塞 IO，应在后台线程调用）。
    pub fn store_file(&self, path: &Path) -> Result<StoredImageMeta, String> {
        let meta = std::fs::metadata(path).map_err(|e| format!("stat {}: {e}", path.display()))?;
        if meta.len() as usize > MAX_IMAGE_BYTES {
            return Err(format!(
                "image file exceeds limit ({} > {} bytes)",
                meta.len(),
                MAX_IMAGE_BYTES
            ));
        }
        let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let hint = path
            .extension()
            .and_then(|ext| ext.to_str())
            .and_then(mime_for_extension);
        self.store_bytes(bytes, hint)
    }

    /// 存入已有 base64 图片（先按 base64 哈希命中缓存，二次遇到零解码）。
    pub fn store_base64(&self, mime: &str, data_b64: &str) -> Result<StoredImageMeta, String> {
        if data_b64.is_empty() {
            return Err("image base64 is empty".into());
        }
        let b64_hash = sha256_bytes(data_b64.as_bytes());
        {
            let guard = self.0.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref_id) = guard.by_b64_hash.get(&b64_hash)
                && let Some(existing) = guard.by_ref.get(ref_id)
            {
                return Ok(existing.meta.clone());
            }
        }
        let bytes = BASE64
            .decode(data_b64.trim())
            .map_err(|e| format!("invalid image base64: {e}"))?;
        let meta = self.store_bytes(bytes, Some(mime))?;
        let mut guard = self.0.lock().unwrap_or_else(|e| e.into_inner());
        guard.by_b64_hash.insert(b64_hash, meta.ref_id.clone());
        Ok(meta)
    }

    /// 取出用于提交给 `smeltd` 的 `AcpImage`（`PromptImage`）。只在锁内拷贝轻指针，构造留到锁外。
    pub fn get_prompt_image(&self, ref_id: &str) -> Option<AcpImage> {
        let (mime, data_b64) = {
            let guard = self.0.lock().unwrap_or_else(|e| e.into_inner());
            let stored = guard.by_ref.get(ref_id)?;
            (stored.meta.mime.clone(), Arc::clone(&stored.data_b64))
        };
        Some(AcpImage::new(mime, data_b64.to_string()))
    }

    /// 取出原图完整 `data:<mime>;base64,...` 供点击放大预览。只在锁内拷贝轻指针，拼接留到锁外。
    pub fn get_data_uri(&self, ref_id: &str) -> Option<String> {
        let (mime, data_b64) = {
            let guard = self.0.lock().unwrap_or_else(|e| e.into_inner());
            let stored = guard.by_ref.get(ref_id)?;
            (stored.meta.mime.clone(), Arc::clone(&stored.data_b64))
        };
        Some(format!("data:{mime};base64,{data_b64}"))
    }

    /// 取出原图 `(mime, bytes)` 供写入系统剪贴板。只在锁内拷贝轻指针，base64 解码留到锁外。
    pub fn get_bytes(&self, ref_id: &str) -> Option<(String, Vec<u8>)> {
        let (mime, data_b64) = {
            let guard = self.0.lock().unwrap_or_else(|e| e.into_inner());
            let stored = guard.by_ref.get(ref_id)?;
            (stored.meta.mime.clone(), Arc::clone(&stored.data_b64))
        };
        let bytes = BASE64.decode(data_b64.as_bytes()).ok()?;
        Some((mime, bytes))
    }

    /// 递归扫描快照 JSON，将大内联 `AcpImage` 落入缓存并替换为轻量 `ref_id` + `preview_uri`。
    pub fn compact_snapshot_json(&self, snapshot: &mut Value) {
        if let Some(entries) = snapshot.get_mut("entries").and_then(Value::as_array_mut) {
            for entry in entries {
                self.compact_entry_json(entry);
            }
        }
        if let Some(images) = snapshot
            .get_mut("composer_restore_images")
            .and_then(Value::as_array_mut)
        {
            for image in images {
                // 输入框还回图片不论大小都必须入库并附带 ref_id / preview_uri，
                // 否则小图（≤ 48 KiB）在 JS 侧无法按 ref_id 重新附回发送。
                self.compact_image_obj(image, true);
            }
        }
    }

    fn compact_entry_json(&self, entry: &mut Value) {
        let Some(obj) = entry.as_object_mut() else {
            return;
        };
        if let Some(user_with_images) = obj.get_mut("UserWithImages").and_then(Value::as_object_mut)
            && let Some(images) = user_with_images
                .get_mut("images")
                .and_then(Value::as_array_mut)
        {
            for image in images {
                self.compact_image_obj(image, false);
            }
        }
        if let Some(tool_call) = obj.get_mut("ToolCall").and_then(Value::as_object_mut) {
            if let Some(output) = tool_call.get_mut("output").and_then(Value::as_array_mut) {
                for part in output {
                    if let Some(img) = part.get_mut("Image") {
                        self.compact_image_obj(img, false);
                    }
                }
            }
            if let Some(children) = tool_call.get_mut("children").and_then(Value::as_array_mut) {
                for child in children {
                    self.compact_entry_json(child);
                }
            }
        }
    }

    fn compact_image_obj(&self, value: &mut Value, force: bool) {
        let Some(obj) = value.as_object_mut() else {
            return;
        };
        let Some(data_b64) = obj.get("data_b64").and_then(Value::as_str) else {
            return;
        };
        if !force && data_b64.len() <= COMPACT_IMAGE_THRESHOLD_B64 {
            return;
        }
        let mime = obj
            .get("mime")
            .and_then(Value::as_str)
            .unwrap_or("image/png");
        let Ok(meta) = self.store_base64(mime, data_b64) else {
            return;
        };
        obj.insert("data_b64".into(), Value::String(String::new()));
        obj.insert("ref_id".into(), Value::String(meta.ref_id));
        obj.insert("preview_uri".into(), Value::String(meta.preview_uri));
        obj.insert("width".into(), Value::from(meta.width));
        obj.insert("height".into(), Value::from(meta.height));
    }
}

/// 解码 base64 图片并校验大小与格式，返回规范化后的 `(mime, bytes)`。
pub fn decode_image_base64(
    mime_hint: Option<&str>,
    data_b64: &str,
) -> Result<(String, Vec<u8>), String> {
    let trimmed = data_b64.trim();
    if trimmed.is_empty() {
        return Err("image base64 is empty".into());
    }
    let bytes = BASE64
        .decode(trimmed)
        .map_err(|e| format!("invalid image base64: {e}"))?;
    if bytes.is_empty() {
        return Err("image is empty".into());
    }
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(format!(
            "image exceeds limit ({} > {} bytes)",
            bytes.len(),
            MAX_IMAGE_BYTES
        ));
    }
    let mime = sniff_mime(&bytes, mime_hint)?;
    Ok((mime, bytes))
}

/// 解码 `data:<mime>;base64,...` 图片 URI，返回规范化后的 `(mime, bytes)`。
pub fn decode_image_data_uri(uri: &str) -> Result<(String, Vec<u8>), String> {
    let Some(rest) = uri.trim().strip_prefix("data:") else {
        return Err("image data URI must start with data:".into());
    };
    let Some((header, payload)) = rest.split_once(',') else {
        return Err("invalid image data URI: missing comma".into());
    };
    let Some(mime_part) = header.strip_suffix(";base64") else {
        return Err("image data URI must be base64 encoded".into());
    };
    let mime_hint = (!mime_part.trim().is_empty()).then_some(mime_part.trim());
    decode_image_base64(mime_hint, payload)
}

fn sha256_bytes(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

fn sha256_hex_16(bytes: &[u8]) -> String {
    let digest = sha256_bytes(bytes);
    let mut hex = String::with_capacity(32);
    for b in &digest[..16] {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

fn mime_for_extension(ext: &str) -> Option<&'static str> {
    match ext.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

fn sniff_mime(bytes: &[u8], hint: Option<&str>) -> Result<String, String> {
    if let Ok(fmt) = image::guess_format(bytes) {
        let mime = match fmt {
            ImageFormat::Png => "image/png",
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::Gif => "image/gif",
            ImageFormat::WebP => "image/webp",
            other => return Err(format!("unsupported image format: {other:?}")),
        };
        return Ok(mime.to_owned());
    }
    if let Some(hint) = hint
        .map(str::trim)
        .filter(|m| matches!(*m, "image/png" | "image/jpeg" | "image/gif" | "image/webp"))
    {
        return Ok(hint.to_owned());
    }
    Err("unrecognized or unsupported image format".into())
}

fn build_preview(bytes: &[u8], mime: &str) -> Result<(u32, u32, String), String> {
    let img = image::load_from_memory(bytes).map_err(|e| format!("decode image failed: {e}"))?;
    let (w, h) = (img.width(), img.height());
    if bytes.len() <= DIRECT_PREVIEW_MAX_BYTES && w <= THUMBNAIL_MAX_SIDE && h <= THUMBNAIL_MAX_SIDE
    {
        return Ok((w, h, format!("data:{mime};base64,{}", BASE64.encode(bytes))));
    }
    let thumb = img.thumbnail(THUMBNAIL_MAX_SIDE, THUMBNAIL_MAX_SIDE);
    let mut out = Cursor::new(Vec::new());
    thumb
        .write_to(&mut out, ImageFormat::Png)
        .map_err(|e| format!("encode thumbnail failed: {e}"))?;
    Ok((
        w,
        h,
        format!("data:image/png;base64,{}", BASE64.encode(out.into_inner())),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn make_png(w: u32, h: u32, color: [u8; 4]) -> Vec<u8> {
        let img = RgbaImage::from_pixel(w, h, Rgba(color));
        let mut buf = Cursor::new(Vec::new());
        img.write_to(&mut buf, ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    #[test]
    fn stores_deduplicates_and_retrieves_images() {
        let store = ImageStore::new();
        let bytes = make_png(32, 24, [10, 20, 30, 255]);
        let m1 = store.store_bytes(bytes.clone(), None).unwrap();
        let m2 = store.store_bytes(bytes, Some("image/png")).unwrap();

        assert_eq!(m1.ref_id, m2.ref_id);
        assert_eq!(m1.mime, "image/png");
        assert_eq!((m1.width, m1.height), (32, 24));
        assert!(m1.preview_uri.starts_with("data:image/png;base64,"));

        let prompt = store.get_prompt_image(&m1.ref_id).unwrap();
        assert_eq!(prompt.mime, "image/png");
        assert!(!prompt.data_b64.is_empty());

        let uri = store.get_data_uri(&m1.ref_id).unwrap();
        assert!(uri.starts_with("data:image/png;base64,"));
    }

    #[test]
    fn downscales_large_image_for_thumbnail_preview() {
        let store = ImageStore::new();
        let bytes = make_png(800, 400, [200, 100, 50, 255]);
        let meta = store.store_bytes(bytes, None).unwrap();
        assert_eq!((meta.width, meta.height), (800, 400));
        let b64 = meta
            .preview_uri
            .strip_prefix("data:image/png;base64,")
            .unwrap();
        let thumb_bytes = BASE64.decode(b64).unwrap();
        let thumb = image::load_from_memory(&thumb_bytes).unwrap();
        assert!(thumb.width() <= THUMBNAIL_MAX_SIDE);
        assert!(thumb.height() <= THUMBNAIL_MAX_SIDE);
    }

    #[test]
    fn rejects_empty_or_invalid_image() {
        let store = ImageStore::new();
        assert!(store.store_bytes(Vec::new(), None).is_err());
        assert!(store.store_bytes(b"not an image".to_vec(), None).is_err());
    }

    #[test]
    fn compacts_large_inline_snapshot_images_in_place() {
        let store = ImageStore::new();
        // 生成噪声大图使 base64 超过 48 KiB 阈值
        let mut img = RgbaImage::new(260, 260);
        for (x, y, px) in img.enumerate_pixels_mut() {
            *px = Rgba([
                ((x * 37 + y * 13) % 256) as u8,
                ((x * 17 + y * 53) % 256) as u8,
                ((x * 71 + y * 29) % 256) as u8,
                255,
            ]);
        }
        let mut buf = Cursor::new(Vec::new());
        img.write_to(&mut buf, ImageFormat::Png).unwrap();
        let big_b64 = BASE64.encode(buf.into_inner());
        assert!(big_b64.len() > COMPACT_IMAGE_THRESHOLD_B64);

        let mut snap = serde_json::json!({
            "entries": [{
                "UserWithImages": {
                    "text": "look",
                    "images": [{ "mime": "image/png", "data_b64": big_b64 }]
                }
            }]
        });
        store.compact_snapshot_json(&mut snap);
        let compacted = &snap["entries"][0]["UserWithImages"]["images"][0];
        assert_eq!(compacted["data_b64"], "");
        let ref_id = compacted["ref_id"].as_str().unwrap();
        assert!(!ref_id.is_empty());
        assert!(
            compacted["preview_uri"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
        assert!(store.get_data_uri(ref_id).is_some());
    }

    #[test]
    fn always_compacts_small_composer_restore_images_with_ref_id() {
        let store = ImageStore::new();
        let small_b64 = BASE64.encode(make_png(16, 16, [10, 20, 30, 255]));
        assert!(small_b64.len() <= COMPACT_IMAGE_THRESHOLD_B64);

        let mut snap = serde_json::json!({
            "composer_restore_images": [{ "mime": "image/png", "data_b64": small_b64 }]
        });
        store.compact_snapshot_json(&mut snap);
        let restored = &snap["composer_restore_images"][0];
        let ref_id = restored["ref_id"].as_str().unwrap();
        assert!(!ref_id.is_empty());
        assert!(
            restored["preview_uri"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
        assert!(store.get_prompt_image(ref_id).is_some());
    }

    #[test]
    fn get_bytes_and_decoders_roundtrip_original_image() {
        let store = ImageStore::new();
        let raw = make_png(24, 18, [12, 34, 56, 255]);
        let meta = store.store_bytes(raw.clone(), Some("image/png")).unwrap();

        let (mime, fetched) = store.get_bytes(&meta.ref_id).unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(fetched, raw);
        assert!(store.get_bytes("missing-ref").is_none());

        let uri = store.get_data_uri(&meta.ref_id).unwrap();
        let (uri_mime, uri_bytes) = decode_image_data_uri(&uri).unwrap();
        assert_eq!(uri_mime, "image/png");
        assert_eq!(uri_bytes, raw);

        let b64 = BASE64.encode(&raw);
        let (b64_mime, b64_bytes) = decode_image_base64(None, &b64).unwrap();
        assert_eq!(b64_mime, "image/png");
        assert_eq!(b64_bytes, raw);

        assert!(decode_image_data_uri("https://example.com/a.png").is_err());
        assert!(decode_image_data_uri("data:image/png,not-base64").is_err());
        assert!(decode_image_base64(Some("image/png"), "").is_err());
    }
}
