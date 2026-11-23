//! 通用本机应用发现、图标读取与目录/文件启动能力（供宿主桥 `os.discover_apps` / `os.launch_path` 调用）。
//!
//! 只做纯系统能力，不认识任何具体编辑器列表或业务插件：
//! - macOS 优先通过 Launch Services 按精确 `.app` 名称定位应用包，并缓存解码后的小尺寸 PNG 图标（`data:image/png;base64,...`）；
//! - 其它平台（以及 macOS 找不到应用包时的回退）在 `PATH` 里查找可执行命令；
//! - 启动始终通过 `Command` 传参、不经过 shell，避免路径中的空格或特殊字符被错误解释。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct DiscoverAppQuery {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) mac_apps: Vec<String>,
    #[serde(default)]
    pub(crate) commands: Vec<String>,
    #[serde(default)]
    pub(crate) with_icon: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct DiscoveredApp {
    pub(crate) id: String,
    pub(crate) kind: &'static str,
    pub(crate) target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) icon: Option<String>,
}

/// 按查询顺序发现本机可用的应用或可执行命令；只在后台线程调用。
pub(crate) fn discover_apps(queries: &[DiscoverAppQuery]) -> Vec<DiscoveredApp> {
    queries
        .iter()
        .filter_map(|query| {
            #[cfg(target_os = "macos")]
            if let Some(app_path) = query.mac_apps.iter().find_map(|app| mac_app_path(app)) {
                let icon = query
                    .with_icon
                    .then(|| mac_app_icon_data_uri_cached(&app_path))
                    .flatten();
                return Some(DiscoveredApp {
                    id: query.id.clone(),
                    kind: "mac_app",
                    target: app_path.to_string_lossy().into_owned(),
                    icon,
                });
            }

            query
                .commands
                .iter()
                .find_map(|command| resolve_command(command))
                .map(|resolved| DiscoveredApp {
                    id: query.id.clone(),
                    kind: "command",
                    target: resolved.to_string_lossy().into_owned(),
                    icon: None,
                })
        })
        .collect()
}

/// 使用已发现的应用或命令打开本机路径，不等待目标进程退出；只在后台线程调用。
pub(crate) fn launch_path(
    kind: &str,
    target: &str,
    path: &str,
    require_dir: bool,
) -> Result<(), String> {
    let target_path = Path::new(path);
    if require_dir {
        if !target_path.is_dir() {
            return Err(format!("directory does not exist: {path}"));
        }
    } else if !target_path.exists() {
        return Err(format!("path does not exist: {path}"));
    }

    let mut command = match kind {
        "mac_app" => {
            #[cfg(target_os = "macos")]
            {
                let mut cmd = Command::new("open");
                cmd.arg("-a").arg(target).arg(target_path);
                cmd
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = target;
                return Err("macOS application bundles are only supported on macOS".to_owned());
            }
        }
        "command" => {
            let mut cmd = Command::new(target);
            cmd.arg(target_path);
            cmd
        }
        other => return Err(format!("unsupported launch kind: {other}")),
    };

    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// 在 `PATH`（Windows 下连同 `PATHEXT` 的 `.exe`/`.cmd`/`.bat` 等）中定位可执行文件，
/// 不回落到当前工作目录，避免误执行项目目录内的同名文件。
fn resolve_command(program: &str) -> Option<PathBuf> {
    resolve_command_in(program, std::env::var_os("PATH"))
}

fn resolve_command_in(program: &str, path_env: Option<std::ffi::OsString>) -> Option<PathBuf> {
    if program.is_empty() {
        return None;
    }
    which::which_in_global(program, path_env).ok()?.next()
}

/// Launch Services 按显示名解析时可能把短名命中到同名扩展包（例如 `Antigravity` 命中
/// `Antigravity IDE.app`）；限定 `.app` 主文件名与查询名精确相等。
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn mac_app_bundle_matches_name(path: &Path, app: &str) -> bool {
    path.file_stem().and_then(|stem| stem.to_str()) == Some(app)
}

#[cfg(target_os = "macos")]
fn mac_app_path(app: &str) -> Option<PathBuf> {
    use objc::rc::autoreleasepool;
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    autoreleasepool(|| unsafe {
        let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
        if workspace.is_null() {
            return None;
        }

        let app_name = ns_string(app)?;
        let app_path: *mut Object = msg_send![workspace, fullPathForApplication: app_name];
        ns_path(app_path).filter(|path| mac_app_bundle_matches_name(path, app))
    })
}

/// 每个已发现的 `.app` 路径只解码并压缩一次为 32×32 PNG `data:` URI，后续直接复用缓存。
#[cfg(target_os = "macos")]
fn mac_app_icon_data_uri_cached(app_path: &Path) -> Option<String> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let cache_key = app_path.to_string_lossy().into_owned();
    if let Some(icon) = cache.lock().ok()?.get(&cache_key) {
        return icon.clone();
    }

    let icon = mac_app_icon_png(app_path)
        .map(|bytes| format!("data:image/png;base64,{}", encode_base64(&bytes)));
    if let Ok(mut entries) = cache.lock() {
        entries.insert(cache_key, icon.clone());
    }
    icon
}

#[cfg(target_os = "macos")]
fn mac_app_icon_png(app_path: &Path) -> Option<Vec<u8>> {
    use objc::rc::autoreleasepool;

    autoreleasepool(|| mac_bundle_icon_png(app_path).or_else(|| mac_workspace_icon_png(app_path)))
}

#[cfg(target_os = "macos")]
fn mac_bundle_icon_path(app_path: &Path) -> Option<PathBuf> {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    unsafe {
        let bundle_path = ns_string(app_path.to_string_lossy().as_ref())?;
        let bundle: *mut Object = msg_send![class!(NSBundle), bundleWithPath: bundle_path];
        if bundle.is_null() {
            return None;
        }

        for key in ["CFBundleIconFile", "CFBundleIconName"] {
            let key = ns_string(key)?;
            let icon_name: *mut Object = msg_send![bundle, objectForInfoDictionaryKey: key];
            let Some(icon_name) = ns_string_value(icon_name) else {
                continue;
            };
            let Some(icon_name) = Path::new(&icon_name).file_name() else {
                continue;
            };

            let icon_path = app_path.join("Contents/Resources").join(icon_name);
            let icon_path = if icon_path.extension().is_some() {
                icon_path
            } else {
                icon_path.with_extension("icns")
            };
            if icon_path.is_file() {
                return Some(icon_path);
            }
        }

        None
    }
}

#[cfg(target_os = "macos")]
fn mac_bundle_icon_png(app_path: &Path) -> Option<Vec<u8>> {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    unsafe {
        let icon_path = mac_bundle_icon_path(app_path)?;
        let icon_path = ns_string(icon_path.to_string_lossy().as_ref())?;
        let image: *mut Object = msg_send![class!(NSImage), alloc];
        if image.is_null() {
            return None;
        }
        let image: *mut Object = msg_send![image, initWithContentsOfFile: icon_path];
        if image.is_null() {
            return None;
        }
        let image: *mut Object = msg_send![image, autorelease];
        mac_ns_image_png(image)
    }
}

#[cfg(target_os = "macos")]
fn mac_workspace_icon_png(app_path: &Path) -> Option<Vec<u8>> {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    unsafe {
        let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
        if workspace.is_null() {
            return None;
        }

        let app_path = ns_string(app_path.to_string_lossy().as_ref())?;
        let image: *mut Object = msg_send![workspace, iconForFile: app_path];
        mac_ns_image_png(image)
    }
}

/// 将任意 `NSImage` 光栅化为 32×32 RGBA PNG（满足 Retina 14-16pt 菜单图标清晰度，同时控制体积在 ~2KB）。
#[cfg(target_os = "macos")]
fn mac_ns_image_png(image: *mut objc::runtime::Object) -> Option<Vec<u8>> {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NSPoint {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NSSize {
        width: f64,
        height: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NSRect {
        origin: NSPoint,
        size: NSSize,
    }

    const ICON_PX: isize = 32;
    const ICON_F64: f64 = 32.0;

    unsafe {
        if image.is_null() {
            return None;
        }
        let color_space = ns_string("NSCalibratedRGBColorSpace")?;
        let bitmap: *mut Object = msg_send![class!(NSBitmapImageRep), alloc];
        if bitmap.is_null() {
            return None;
        }
        let bitmap: *mut Object = msg_send![
            bitmap,
            initWithBitmapDataPlanes: std::ptr::null_mut::<*mut u8>()
            pixelsWide: ICON_PX
            pixelsHigh: ICON_PX
            bitsPerSample: 8isize
            samplesPerPixel: 4isize
            hasAlpha: true
            isPlanar: false
            colorSpaceName: color_space
            bytesPerRow: 0isize
            bitsPerPixel: 0isize
        ];
        if bitmap.is_null() {
            return None;
        }
        let bitmap: *mut Object = msg_send![bitmap, autorelease];
        let _: () = msg_send![bitmap, setSize: NSSize { width: ICON_F64, height: ICON_F64 }];

        let ctx: *mut Object =
            msg_send![class!(NSGraphicsContext), graphicsContextWithBitmapImageRep: bitmap];
        if !ctx.is_null() {
            let _: () = msg_send![class!(NSGraphicsContext), saveGraphicsState];
            let _: () = msg_send![class!(NSGraphicsContext), setCurrentContext: ctx];
            // NSImageInterpolationHigh = 3
            let _: () = msg_send![ctx, setImageInterpolation: 3usize];
            let dest = NSRect {
                origin: NSPoint { x: 0.0, y: 0.0 },
                size: NSSize {
                    width: ICON_F64,
                    height: ICON_F64,
                },
            };
            let src = NSRect {
                origin: NSPoint { x: 0.0, y: 0.0 },
                size: NSSize {
                    width: 0.0,
                    height: 0.0,
                },
            };
            // NSCompositingOperationCopy = 1
            let _: () = msg_send![
                image,
                drawInRect: dest
                fromRect: src
                operation: 1usize
                fraction: 1.0f64
            ];
            let _: () = msg_send![class!(NSGraphicsContext), restoreGraphicsState];
        } else {
            return mac_ns_image_png_direct(image);
        }

        let properties: *mut Object = std::ptr::null_mut();
        // NSPNGFileType = 4
        let png: *mut Object = msg_send![bitmap,
            representationUsingType: 4usize
            properties: properties
        ];
        if png.is_null() {
            return None;
        }
        let bytes: *const u8 = msg_send![png, bytes];
        let length: usize = msg_send![png, length];
        if bytes.is_null() || length == 0 {
            return None;
        }
        Some(std::slice::from_raw_parts(bytes, length).to_vec())
    }
}

#[cfg(target_os = "macos")]
unsafe fn mac_ns_image_png_direct(image: *mut objc::runtime::Object) -> Option<Vec<u8>> {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    let tiff: *mut Object = msg_send![image, TIFFRepresentation];
    if tiff.is_null() {
        return None;
    }
    let bitmap: *mut Object = msg_send![class!(NSBitmapImageRep), imageRepWithData: tiff];
    if bitmap.is_null() {
        return None;
    }
    let properties: *mut Object = std::ptr::null_mut();
    let png: *mut Object = msg_send![bitmap,
        representationUsingType: 4usize
        properties: properties
    ];
    if png.is_null() {
        return None;
    }
    let bytes: *const u8 = msg_send![png, bytes];
    let length: usize = msg_send![png, length];
    if bytes.is_null() || length == 0 {
        return None;
    }
    Some(unsafe { std::slice::from_raw_parts(bytes, length) }.to_vec())
}

#[cfg(target_os = "macos")]
unsafe fn ns_string(value: &str) -> Option<*mut objc::runtime::Object> {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    let c_string = std::ffi::CString::new(value).ok()?;
    let string: *mut Object = msg_send![class!(NSString), stringWithUTF8String: c_string.as_ptr()];
    (!string.is_null()).then_some(string)
}

#[cfg(target_os = "macos")]
unsafe fn ns_path(value: *mut objc::runtime::Object) -> Option<PathBuf> {
    unsafe { ns_string_value(value) }.map(PathBuf::from)
}

#[cfg(target_os = "macos")]
unsafe fn ns_string_value(value: *mut objc::runtime::Object) -> Option<String> {
    use objc::{msg_send, sel, sel_impl};

    if value.is_null() {
        return None;
    }
    let bytes: *const std::os::raw::c_char = msg_send![value, UTF8String];
    if bytes.is_null() {
        return None;
    }
    let path = unsafe { std::ffi::CStr::from_ptr(bytes) }
        .to_string_lossy()
        .into_owned();
    (!path.is_empty()).then_some(path)
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn encode_base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((n >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_encodes_rfc4648_vectors() {
        assert_eq!(encode_base64(b""), "");
        assert_eq!(encode_base64(b"f"), "Zg==");
        assert_eq!(encode_base64(b"fo"), "Zm8=");
        assert_eq!(encode_base64(b"foo"), "Zm9v");
        assert_eq!(encode_base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn mac_app_bundle_matches_exact_stem() {
        assert!(mac_app_bundle_matches_name(
            Path::new("/Applications/Antigravity.app"),
            "Antigravity"
        ));
        assert!(!mac_app_bundle_matches_name(
            Path::new("/Applications/Antigravity IDE.app"),
            "Antigravity"
        ));
        assert!(mac_app_bundle_matches_name(
            Path::new("/Applications/Antigravity IDE.app"),
            "Antigravity IDE"
        ));
    }

    #[test]
    fn path_lookup_finds_rustc() {
        assert!(resolve_command("rustc").is_some());
        assert!(resolve_command("smelt-command-that-does-not-exist").is_none());
    }

    #[test]
    fn discover_command_returns_resolved_executable_path() {
        let queries = vec![DiscoverAppQuery {
            id: "rustc".into(),
            mac_apps: vec![],
            commands: vec!["rustc".into()],
            with_icon: false,
        }];
        let found = discover_apps(&queries);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, "command");
        assert!(
            Path::new(&found[0].target).is_absolute(),
            "command target 应为解析后的完整路径（Windows 上才能直接启动 .cmd/.bat），实得：{:?}",
            found[0].target
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_command_in_skips_directories_and_non_executable_files() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("smelt-which-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("dir_cmd")).unwrap();
        std::fs::write(dir.join("non_exec"), b"#!/bin/sh\n").unwrap();
        let exec = dir.join("my_ide");
        std::fs::write(&exec, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&exec, std::fs::Permissions::from_mode(0o755)).unwrap();

        let path_env = Some(dir.as_os_str().to_os_string());
        assert_eq!(resolve_command_in("dir_cmd", path_env.clone()), None);
        assert_eq!(resolve_command_in("non_exec", path_env.clone()), None);
        assert_eq!(resolve_command_in("my_ide", path_env), Some(exec));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn finder_discovery_returns_cached_png_data_uri() {
        let queries = vec![DiscoverAppQuery {
            id: "finder".into(),
            mac_apps: vec!["Finder".into()],
            commands: vec![],
            with_icon: true,
        }];
        let first = discover_apps(&queries);
        if first.is_empty() {
            return;
        }
        assert_eq!(first[0].kind, "mac_app");
        let icon = first[0]
            .icon
            .as_deref()
            .expect("Finder 应返回 32x32 PNG data URI");
        assert!(
            icon.starts_with("data:image/png;base64,iVBORw0KGgo"),
            "PNG data URI 头不对：{icon}"
        );
        let second = discover_apps(&queries);
        assert_eq!(first, second);
    }
}
