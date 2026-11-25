//! `terminal.stage_image`：把移动端 / 远程客户端传来的图片落盘到运行时临时目录，
//! 返回本机绝对路径供 PTY 里的 CLI（如 agy、claude、codex）读取。
//!
//! 没有常驻定时器：仅在守护启动和每次暂存图片时惰性清理过期或超量文件。

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use smelt_runtime_client::protocol::control::{ControlError, ControlErrorCode};
use smelt_runtime_client::protocol::terminal::{
    MAX_TERMINAL_IMAGE_BYTES, TerminalStageImageParams, TerminalStageImageResult,
};

use super::Sessions;

const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_FILES: usize = 100;
const MAX_TOTAL_BYTES: u64 = 200 * 1024 * 1024;
const FILE_PREFIX: &str = "img-";

pub(crate) fn staging_dir() -> PathBuf {
    #[cfg(unix)]
    {
        std::env::temp_dir().join(format!("smelt-terminal-images-{}", unsafe {
            libc::geteuid()
        }))
    }
    #[cfg(not(unix))]
    {
        std::env::temp_dir().join("smelt-terminal-images")
    }
}

pub(crate) fn cleanup_expired_images() {
    cleanup_in(
        &staging_dir(),
        SystemTime::now(),
        MAX_AGE,
        MAX_FILES,
        MAX_TOTAL_BYTES,
    );
}

pub(crate) fn stage_image(
    params: TerminalStageImageParams,
    sessions: &Sessions,
) -> Result<TerminalStageImageResult, ControlError> {
    if params.session_id.trim().is_empty() {
        return Err(ControlError::new(
            ControlErrorCode::InvalidParams,
            "session_id can't be empty",
        ));
    }
    if sessions.with_live(&params.session_id, |_| ()).is_none() {
        return Err(ControlError::new(
            ControlErrorCode::NotFound,
            "No such terminal session",
        ));
    }
    let path = stage_image_in(
        &staging_dir(),
        &params.mime,
        &params.data.0,
        SystemTime::now(),
    )?;
    Ok(TerminalStageImageResult {
        path: path.to_string_lossy().into_owned(),
    })
}

fn detect_extension(mime: &str, bytes: &[u8]) -> Result<&'static str, ControlError> {
    let normalized = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match normalized.as_str() {
        "image/png" | "image/jpeg" | "image/jpg" | "image/gif" | "image/webp" => {}
        _ => {
            return Err(ControlError::new(
                ControlErrorCode::InvalidParams,
                format!("unsupported image MIME type: {mime}"),
            ));
        }
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Ok("png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Ok("jpg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Ok("gif")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Ok("webp")
    } else {
        Err(ControlError::new(
            ControlErrorCode::InvalidParams,
            "invalid image payload",
        ))
    }
}

fn ensure_private_dir(root: &Path) -> Result<(), ControlError> {
    fs::create_dir_all(root).map_err(|error| {
        ControlError::new(
            ControlErrorCode::OperationFailed,
            format!("couldn't prepare staging directory: {error}"),
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let meta = fs::symlink_metadata(root).map_err(|error| {
            ControlError::new(
                ControlErrorCode::OperationFailed,
                format!("couldn't inspect staging directory: {error}"),
            )
        })?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(ControlError::new(
                ControlErrorCode::OperationFailed,
                "staging directory path is not a directory",
            ));
        }
        if meta.uid() != unsafe { libc::geteuid() } {
            return Err(ControlError::new(
                ControlErrorCode::OperationFailed,
                "staging directory is owned by another user",
            ));
        }
        let _ = fs::set_permissions(root, fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

fn stage_image_in(
    root: &Path,
    mime: &str,
    bytes: &[u8],
    now: SystemTime,
) -> Result<PathBuf, ControlError> {
    if bytes.is_empty() {
        return Err(ControlError::new(
            ControlErrorCode::InvalidParams,
            "image data can't be empty",
        ));
    }
    if bytes.len() > MAX_TERMINAL_IMAGE_BYTES {
        return Err(ControlError::new(
            ControlErrorCode::InvalidParams,
            "image exceeds 5 MiB limit",
        ));
    }
    let ext = detect_extension(mime, bytes)?;
    ensure_private_dir(root)?;
    cleanup_in(
        root,
        now,
        MAX_AGE,
        MAX_FILES.saturating_sub(1),
        MAX_TOTAL_BYTES.saturating_sub(bytes.len() as u64),
    );

    let path = root.join(format!(
        "{FILE_PREFIX}{}.{ext}",
        uuid::Uuid::new_v4().simple()
    ));
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let write_result = opts.open(&path).and_then(|mut file| {
        file.write_all(bytes)?;
        file.flush()
    });
    if let Err(error) = write_result {
        let _ = fs::remove_file(&path);
        return Err(ControlError::new(
            ControlErrorCode::OperationFailed,
            format!("couldn't write staged image: {error}"),
        ));
    }
    Ok(path)
}

fn cleanup_in(
    root: &Path,
    now: SystemTime,
    max_age: Duration,
    max_files: usize,
    max_total_bytes: u64,
) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let mut kept: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
    let mut total_bytes = 0u64;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if !name_str.starts_with(FILE_PREFIX) {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() || !meta.is_file() {
            continue;
        }
        let modified = meta.modified().unwrap_or(now);
        if now.duration_since(modified).is_ok_and(|age| age > max_age) {
            let _ = fs::remove_file(&path);
            continue;
        }
        let size = meta.len();
        total_bytes = total_bytes.saturating_add(size);
        kept.push((modified, size, path));
    }
    if kept.len() <= max_files && total_bytes <= max_total_bytes {
        return;
    }
    kept.sort_by_key(|(modified, _, _)| *modified);
    let mut remaining = kept.len();
    for (_, size, path) in kept {
        if remaining <= max_files && total_bytes <= max_total_bytes {
            break;
        }
        let _ = fs::remove_file(&path);
        remaining -= 1;
        total_bytes = total_bytes.saturating_sub(size);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDirGuard(PathBuf);

    impl TempDirGuard {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "smelt-stage-img-test-{}",
                uuid::Uuid::new_v4().simple()
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const PNG_BYTES: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00\x1f\x15c4";
    const JPEG_BYTES: &[u8] =
        b"\xFF\xD8\xFF\xE0\x00\x10JFIF\x00\x01\x01\x00\x00\x01\x00\x01\x00\x00";
    const GIF_BYTES: &[u8] = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;";
    const WEBP_BYTES: &[u8] = b"RIFF\x1a\x00\x00\x00WEBPVP8 \x0e\x00\x00\x00\x00\x00\x00\x00";

    #[test]
    fn stages_valid_images_with_matching_extensions_and_private_permissions() {
        let dir = TempDirGuard::new();
        let now = SystemTime::now();

        for (mime, bytes, expected_ext) in [
            ("image/png", PNG_BYTES, "png"),
            ("image/jpeg; charset=binary", JPEG_BYTES, "jpg"),
            ("image/gif", GIF_BYTES, "gif"),
            ("image/webp", WEBP_BYTES, "webp"),
        ] {
            let path = stage_image_in(&dir.0, mime, bytes, now).unwrap();
            assert_eq!(
                path.extension().and_then(|e| e.to_str()),
                Some(expected_ext)
            );
            assert_eq!(fs::read(&path).unwrap(), bytes);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600);
            }
        }
    }

    #[test]
    fn rejects_empty_oversized_unsupported_mime_and_bad_magic() {
        let dir = TempDirGuard::new();
        let now = SystemTime::now();

        let empty = stage_image_in(&dir.0, "image/png", &[], now).unwrap_err();
        assert_eq!(empty.code, ControlErrorCode::InvalidParams);

        let oversized = vec![0u8; MAX_TERMINAL_IMAGE_BYTES + 1];
        let too_big = stage_image_in(&dir.0, "image/png", &oversized, now).unwrap_err();
        assert_eq!(too_big.code, ControlErrorCode::InvalidParams);

        for bad_mime in ["image/heic", "text/plain", "application/octet-stream"] {
            let err = stage_image_in(&dir.0, bad_mime, PNG_BYTES, now).unwrap_err();
            assert_eq!(err.code, ControlErrorCode::InvalidParams, "{bad_mime}");
        }

        let bad_magic =
            stage_image_in(&dir.0, "image/png", b"not-actually-a-png", now).unwrap_err();
        assert_eq!(bad_magic.code, ControlErrorCode::InvalidParams);
    }

    #[test]
    fn cleanup_removes_expired_and_excess_files_while_keeping_unrelated_ones() {
        let dir = TempDirGuard::new();
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

        let write_at = |name: &str, size: usize, mtime: SystemTime| {
            let path = dir.0.join(name);
            let mut f = fs::File::create(&path).unwrap();
            f.write_all(&vec![b'x'; size]).unwrap();
            f.set_modified(mtime).unwrap();
            path
        };

        let unrelated = write_at("keep-me.txt", 32, base);
        let expired = write_at("img-expired.png", 10, base);
        let oldest = write_at("img-oldest.png", 10, base + Duration::from_secs(100));
        let middle = write_at("img-middle.png", 10, base + Duration::from_secs(200));
        let newest = write_at("img-newest.png", 10, base + Duration::from_secs(300));

        // 检查 TTL：过期删掉，其余按数量上限 2 删最旧的。
        let now = base + Duration::from_secs(600);
        cleanup_in(&dir.0, now, Duration::from_secs(550), 2, 1024);
        assert!(unrelated.exists(), "不删非 img- 前缀的文件");
        assert!(!expired.exists());
        assert!(!oldest.exists());
        assert!(middle.exists());
        assert!(newest.exists());

        // 再按总字节上限 12 清理：middle 被删，只留 newest。
        cleanup_in(&dir.0, now, Duration::from_secs(550), 100, 12);
        assert!(!middle.exists());
        assert!(newest.exists());
    }
}
