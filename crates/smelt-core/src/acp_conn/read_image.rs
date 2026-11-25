//! ACP `read` 工具的图片预览回填。
//!
//! 部分 ACP agent（如 Antigravity `agy acp`）在 `read_file` 的 `tool_call` /
//! `tool_call_update` 里只上报 `locations` / `rawInput` 路径，不下发 `content`；
//! 导致对话归约出的 `ToolCall.output` 为空，界面上该行不可点击也无预览。
//!
//! 这里在 ACP 连接层按 `tool_call_id` 跟踪进行中的 `read` 工具：当它以 `Completed`
//! 收尾且 agent 全程未提供非空 `content` 时，若目标路径是本机可读的可预览图片，
//! 则读取原图并补成标准的 `ContentBlock::Image`，下游桌面端、远程与移动端自动复用
//! 同一套图片预览与按引用传输链路。

use std::collections::{HashMap, HashSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};

use agent_client_protocol::v1::schema::{
    ContentBlock, ImageContent, ToolCall, ToolCallContent, ToolCallLocation, ToolCallStatus,
    ToolCallUpdate, ToolKind,
};
use base64::Engine as _;
use smelt_runtime_client::protocol::files::PREVIEWABLE_IMAGE_EXTENSIONS;

use crate::project_files::IMAGE_OPEN_LIMIT;

/// `tracked` 的软上限：长会话超过该条数时先清理已收尾的记录，避免内存单调增长。
const MAX_TRACKED_TOOLS: usize = 4096;

/// `rawInput` 里常见的文件路径字段名（兼容不同 ACP agent 方言）。
const RAW_INPUT_PATH_KEYS: [&str; 6] = [
    "Path",
    "path",
    "file_path",
    "target_file",
    "TargetFile",
    "AbsolutePath",
];

/// 按单条 ACP 连接跟踪 `read` 工具并在缺 `content` 时回填图片块。
pub(super) struct ReadImageHydrator {
    cwd: PathBuf,
    tracked: HashMap<String, TrackedTool>,
}

#[derive(Default)]
struct TrackedTool {
    kind: ToolKind,
    paths: Vec<PathBuf>,
    /// 已经收到非空 `content`、已尝试回填、或已失败：后续重复更新不再读盘。
    settled: bool,
}

impl ReadImageHydrator {
    pub(super) fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            tracked: HashMap::new(),
        }
    }

    /// 处理一条 `tool_call` 通知。`session/load` 重放常直接发送 `Completed` 的完整调用。
    pub(super) fn hydrate_tool_call(&mut self, call: &mut ToolCall) {
        let id = call.tool_call_id.to_string();
        let entry = self.entry_mut(id);
        entry.kind = call.kind;
        let paths = extract_paths(&call.locations, call.raw_input.as_ref());
        if !paths.is_empty() {
            entry.paths = paths;
        }
        if !call.content.is_empty() || matches!(call.status, ToolCallStatus::Failed) {
            entry.settled = true;
            entry.paths.clear();
            return;
        }
        if entry.settled || call.status != ToolCallStatus::Completed || entry.kind != ToolKind::Read
        {
            return;
        }
        entry.settled = true;
        let candidate_paths = std::mem::take(&mut entry.paths);
        let images = load_previewable_images(&candidate_paths, &self.cwd);
        if !images.is_empty() {
            call.content = images;
        }
    }

    /// 处理一条稀疏的 `tool_call_update` 通知。
    pub(super) fn hydrate_tool_call_update(&mut self, update: &mut ToolCallUpdate) {
        let id = update.tool_call_id.to_string();
        let entry = self.entry_mut(id);
        if let Some(kind) = update.fields.kind {
            entry.kind = kind;
        }
        if let Some(locations) = update.fields.locations.as_deref() {
            let paths = extract_paths(locations, update.fields.raw_input.as_ref());
            if !paths.is_empty() {
                entry.paths = paths;
            }
        } else if entry.paths.is_empty()
            && let Some(raw_input) = update.fields.raw_input.as_ref()
        {
            let paths = paths_from_raw_input(raw_input);
            if !paths.is_empty() {
                entry.paths = paths;
            }
        }
        let has_non_empty_content = update
            .fields
            .content
            .as_deref()
            .is_some_and(|parts| !parts.is_empty());
        if has_non_empty_content || update.fields.status == Some(ToolCallStatus::Failed) {
            entry.settled = true;
            entry.paths.clear();
            return;
        }
        if entry.settled
            || update.fields.status != Some(ToolCallStatus::Completed)
            || entry.kind != ToolKind::Read
        {
            return;
        }
        entry.settled = true;
        let candidate_paths = std::mem::take(&mut entry.paths);
        let images = load_previewable_images(&candidate_paths, &self.cwd);
        if !images.is_empty() {
            update.fields.content = Some(images);
        }
    }

    fn entry_mut(&mut self, id: String) -> &mut TrackedTool {
        if !self.tracked.contains_key(&id) && self.tracked.len() >= MAX_TRACKED_TOOLS {
            self.tracked.retain(|_, tool| !tool.settled);
        }
        self.tracked.entry(id).or_default()
    }
}

fn extract_paths(
    locations: &[ToolCallLocation],
    raw_input: Option<&serde_json::Value>,
) -> Vec<PathBuf> {
    if !locations.is_empty() {
        return locations
            .iter()
            .map(|loc| normalize_raw_path(&loc.path))
            .collect();
    }
    raw_input.map(paths_from_raw_input).unwrap_or_default()
}

fn paths_from_raw_input(raw_input: &serde_json::Value) -> Vec<PathBuf> {
    let Some(object) = raw_input.as_object() else {
        return Vec::new();
    };
    RAW_INPUT_PATH_KEYS
        .iter()
        .filter_map(|key| object.get(*key).and_then(serde_json::Value::as_str))
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(|path| PathBuf::from(crate::workspace_override::expand_tilde(path)))
        .collect()
}

fn normalize_raw_path(path: &Path) -> PathBuf {
    match path.to_str() {
        Some(text) if text.starts_with('~') => {
            PathBuf::from(crate::workspace_override::expand_tilde(text))
        }
        _ => path.to_path_buf(),
    }
}

fn load_previewable_images(paths: &[PathBuf], cwd: &Path) -> Vec<ToolCallContent> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for raw in paths {
        let resolved = if raw.is_absolute() {
            raw.clone()
        } else {
            cwd.join(raw)
        };
        if !seen.insert(resolved.clone()) {
            continue;
        }
        if let Some(block) = read_image_content(&resolved) {
            out.push(block);
        }
    }
    out
}

fn read_image_content(path: &Path) -> Option<ToolCallContent> {
    if !has_previewable_image_extension(path) {
        return None;
    }
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > IMAGE_OPEN_LIMIT {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    let capacity = usize::try_from(meta.len()).unwrap_or(0);
    let mut bytes = Vec::with_capacity(capacity);
    file.take(IMAGE_OPEN_LIMIT.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.is_empty() || bytes.len() as u64 > IMAGE_OPEN_LIMIT {
        return None;
    }
    let mime = detect_image_mime(&bytes, path)?;
    let data_b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Some(ToolCallContent::from(ContentBlock::Image(
        ImageContent::new(data_b64, mime),
    )))
}

fn has_previewable_image_extension(path: &Path) -> bool {
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return false;
    };
    if stem.is_empty() {
        return false;
    }
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    PREVIEWABLE_IMAGE_EXTENSIONS
        .iter()
        .any(|known| ext.eq_ignore_ascii_case(known))
}

/// 按文件魔数判断实际图片 MIME，避免扩展名与实际编码不一致（例如 `.jpg` 实为 PNG）时
/// 导致桌面端按格式严格解码失败。非有效图片内容返回 `None`。
fn detect_image_mime(bytes: &[u8], path: &Path) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() >= 14 && bytes.starts_with(b"BM") {
        return Some("image/bmp");
    }
    if bytes.starts_with(&[b'I', b'I', 0x2A, 0x00]) || bytes.starts_with(&[b'M', b'M', 0x00, 0x2A])
    {
        return Some("image/tiff");
    }
    let is_svg_ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"));
    if is_svg_ext && looks_like_svg(bytes) {
        return Some("image/svg+xml");
    }
    None
}

fn looks_like_svg(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    if !trimmed.starts_with('<') {
        return false;
    }
    let cap = trimmed.len().min(4096);
    let head_end = (0..=cap)
        .rev()
        .find(|&idx| trimmed.is_char_boundary(idx))
        .unwrap_or(0);
    trimmed[..head_end].to_ascii_lowercase().contains("<svg")
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::v1::schema::ToolCallUpdateFields;
    use serde_json::json;

    const PNG_BYTES: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";
    const JPEG_BYTES: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F'];

    fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("write test file");
        path
    }

    fn extract_images(content: &[ToolCallContent]) -> Vec<(&str, &str)> {
        content
            .iter()
            .filter_map(|part| match part {
                ToolCallContent::Content(inner) => match &inner.content {
                    ContentBlock::Image(im) => Some((im.mime_type.as_str(), im.data.as_str())),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    #[test]
    fn hydrates_completed_update_for_live_image_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_file(dir.path(), "shot.jpg", JPEG_BYTES);
        let mut hydrator = ReadImageHydrator::new(dir.path().to_path_buf());

        let mut call = ToolCall::new("t1", "File shot.jpg")
            .kind(ToolKind::Read)
            .status(ToolCallStatus::InProgress)
            .locations(vec![ToolCallLocation::new(path.clone())]);
        hydrator.hydrate_tool_call(&mut call);
        assert!(call.content.is_empty(), "进行中不应提前读盘");

        // 稀疏更新：不重复 kind / locations，只上报 completed。
        let mut update = ToolCallUpdate::new(
            "t1",
            ToolCallUpdateFields::new().status(ToolCallStatus::Completed),
        );
        hydrator.hydrate_tool_call_update(&mut update);

        let expected = base64::engine::general_purpose::STANDARD.encode(JPEG_BYTES);
        assert_eq!(
            extract_images(update.fields.content.as_deref().unwrap_or(&[])),
            vec![("image/jpeg", expected.as_str())]
        );

        // 重复的 Completed 更新不重复回填。
        let mut duplicate = ToolCallUpdate::new(
            "t1",
            ToolCallUpdateFields::new()
                .kind(ToolKind::Read)
                .status(ToolCallStatus::Completed)
                .locations(vec![ToolCallLocation::new(path)]),
        );
        hydrator.hydrate_tool_call_update(&mut duplicate);
        assert!(duplicate.fields.content.is_none());
    }

    #[test]
    fn hydrates_completed_tool_call_during_replay() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_file(dir.path(), "preview.png", PNG_BYTES);
        let mut hydrator = ReadImageHydrator::new(dir.path().to_path_buf());

        let mut call = ToolCall::new("t-replay", "File preview.png")
            .kind(ToolKind::Read)
            .status(ToolCallStatus::Completed)
            .locations(vec![ToolCallLocation::new(path)]);
        hydrator.hydrate_tool_call(&mut call);

        let expected = base64::engine::general_purpose::STANDARD.encode(PNG_BYTES);
        assert_eq!(
            extract_images(&call.content),
            vec![("image/png", expected.as_str())]
        );
    }

    #[test]
    fn falls_back_to_raw_input_and_resolves_relative_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_file(dir.path(), "rel.png", PNG_BYTES);
        let mut hydrator = ReadImageHydrator::new(dir.path().to_path_buf());

        let mut call = ToolCall::new("t-rel", "File rel.png")
            .kind(ToolKind::Read)
            .status(ToolCallStatus::Completed)
            .raw_input(json!({"Path": "rel.png"}));
        hydrator.hydrate_tool_call(&mut call);

        let expected = base64::engine::general_purpose::STANDARD.encode(PNG_BYTES);
        assert_eq!(
            extract_images(&call.content),
            vec![("image/png", expected.as_str())]
        );
    }

    #[test]
    fn sniffs_real_mime_when_extension_mismatches_payload() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_file(dir.path(), "actually_png.jpg", PNG_BYTES);
        let mut hydrator = ReadImageHydrator::new(dir.path().to_path_buf());

        let mut call = ToolCall::new("t-mime", "File actually_png.jpg")
            .kind(ToolKind::Read)
            .status(ToolCallStatus::Completed)
            .locations(vec![ToolCallLocation::new(path)]);
        hydrator.hydrate_tool_call(&mut call);

        let expected = base64::engine::general_purpose::STANDARD.encode(PNG_BYTES);
        assert_eq!(
            extract_images(&call.content),
            vec![("image/png", expected.as_str())]
        );
    }

    #[test]
    fn handles_valid_svg_and_rejects_corrupt_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let svg_path = write_file(
            dir.path(),
            "icon.svg",
            br#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg"></svg>"#,
        );
        let fake_jpg = write_file(dir.path(), "broken.jpg", b"<html>404</html>");
        let fake_svg = write_file(dir.path(), "broken.svg", b"not xml at all");
        let code_file = write_file(dir.path(), "main.rs", b"fn main() {}");
        let mut hydrator = ReadImageHydrator::new(dir.path().to_path_buf());

        let mut svg_call = ToolCall::new("t-svg", "File icon.svg")
            .kind(ToolKind::Read)
            .status(ToolCallStatus::Completed)
            .locations(vec![ToolCallLocation::new(svg_path)]);
        hydrator.hydrate_tool_call(&mut svg_call);
        assert_eq!(extract_images(&svg_call.content).len(), 1);
        assert_eq!(extract_images(&svg_call.content)[0].0, "image/svg+xml");

        for (id, path) in [
            ("t-fake-jpg", fake_jpg),
            ("t-fake-svg", fake_svg),
            ("t-code", code_file),
            ("t-missing", dir.path().join("missing.png")),
            ("t-dir", dir.path().to_path_buf()),
        ] {
            let mut call = ToolCall::new(id, "File x")
                .kind(ToolKind::Read)
                .status(ToolCallStatus::Completed)
                .locations(vec![ToolCallLocation::new(path)]);
            hydrator.hydrate_tool_call(&mut call);
            assert!(
                call.content.is_empty(),
                "{id} 不应产出图片块：{:?}",
                call.content
            );
        }
    }

    #[test]
    fn skips_failed_non_read_and_agent_provided_content() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_file(dir.path(), "pic.png", PNG_BYTES);
        let mut hydrator = ReadImageHydrator::new(dir.path().to_path_buf());

        // 失败的 read 不回填。
        let mut failed = ToolCall::new("t-fail", "File pic.png")
            .kind(ToolKind::Read)
            .status(ToolCallStatus::Failed)
            .locations(vec![ToolCallLocation::new(path.clone())]);
        hydrator.hydrate_tool_call(&mut failed);
        assert!(failed.content.is_empty());

        // 非 Read 工具不回填。
        let mut edit = ToolCall::new("t-edit", "File pic.png")
            .kind(ToolKind::Edit)
            .status(ToolCallStatus::Completed)
            .locations(vec![ToolCallLocation::new(path.clone())]);
        hydrator.hydrate_tool_call(&mut edit);
        assert!(edit.content.is_empty());

        // agent 已经提供过 content 时，后续 Completed 不覆盖也不重复回填。
        let mut with_content = ToolCall::new("t-agent", "File pic.png")
            .kind(ToolKind::Read)
            .status(ToolCallStatus::InProgress)
            .locations(vec![ToolCallLocation::new(path)])
            .content(vec![ToolCallContent::from(ContentBlock::Image(
                ImageContent::new("QUJD", "image/png"),
            ))]);
        hydrator.hydrate_tool_call(&mut with_content);
        let mut done = ToolCallUpdate::new(
            "t-agent",
            ToolCallUpdateFields::new().status(ToolCallStatus::Completed),
        );
        hydrator.hydrate_tool_call_update(&mut done);
        assert!(done.fields.content.is_none());
    }

    #[test]
    fn deduplicates_identical_locations() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_file(dir.path(), "dup.png", PNG_BYTES);
        let mut hydrator = ReadImageHydrator::new(dir.path().to_path_buf());

        let mut call = ToolCall::new("t-dup", "File dup.png")
            .kind(ToolKind::Read)
            .status(ToolCallStatus::Completed)
            .locations(vec![
                ToolCallLocation::new(path.clone()),
                ToolCallLocation::new(path),
            ]);
        hydrator.hydrate_tool_call(&mut call);
        assert_eq!(extract_images(&call.content).len(), 1);
    }
}
