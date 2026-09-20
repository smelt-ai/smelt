//! 本地 IDE 探测、应用图标缓存与项目目录启动。
//!
//! 项目行的快捷入口只负责展示本机实际可用的编辑器：macOS 优先通过 Launch
//! Services 探测 `.app`，优先读取应用包声明的彩色图标；其他平台使用 PATH 中的 CLI。
//! 探测可能需要访问系统注册表和文件系统，必须在后台执行；菜单 render 只消费
//! `IdeCatalog` 已有的快照。启动时始终通过 `Command` 传递参数，不经过 shell，避免
//! 项目路径中的空格或特殊字符被错误解释。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

/// Workspace 生命周期内的 IDE 发现快照。
///
/// `installed: None` 表示尚未得到过结果，`Some(vec![])` 则是已经完成扫描但确实
/// 没有匹配项。刷新时保留旧快照，让菜单继续可用而不是退回加载态。
#[derive(Default)]
pub(crate) struct IdeCatalog {
    installed: Option<Vec<InstalledIde>>,
    file_manager_icon: Option<Arc<gpui::Image>>,
    file_manager_icon_loading: bool,
    file_manager_icon_loaded: bool,
    scanning: bool,
    idle_prewarm_scheduled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct IdeCatalogSnapshot {
    pub(crate) installed: Option<Vec<InstalledIde>>,
    pub(crate) file_manager_icon: Option<Arc<gpui::Image>>,
    pub(crate) scanning: bool,
}

impl IdeCatalog {
    /// 取得可跨出可变借用使用的菜单快照。
    pub(crate) fn snapshot(&self) -> IdeCatalogSnapshot {
        IdeCatalogSnapshot {
            installed: self.installed.clone(),
            file_manager_icon: self.file_manager_icon.clone(),
            scanning: self.scanning,
        }
    }

    /// 标记一轮发现开始。已经在扫描时返回 false，避免多个菜单重复起后台任务。
    pub(crate) fn begin_refresh(&mut self) -> bool {
        if self.scanning {
            return false;
        }
        self.scanning = true;
        self.idle_prewarm_scheduled = false;
        true
    }

    /// 原子替换结果；即使结果为空，也要记成已扫描，避免每次打开都重新探测。
    pub(crate) fn finish_refresh(&mut self, installed: Vec<InstalledIde>) {
        self.installed = Some(installed);
        self.scanning = false;
    }

    /// Finder 图标单独预取：它是一个已知系统应用，不需要等整份应用列表发现完成。
    pub(crate) fn begin_file_manager_icon_load(&mut self) -> bool {
        if self.file_manager_icon_loaded || self.file_manager_icon_loading {
            return false;
        }
        self.file_manager_icon_loading = true;
        true
    }

    pub(crate) fn finish_file_manager_icon_load(&mut self, icon: Option<Arc<gpui::Image>>) {
        self.file_manager_icon = icon;
        self.file_manager_icon_loading = false;
        self.file_manager_icon_loaded = true;
    }

    /// 首屏绘制后只预约一次低优先级预热。菜单先被点开时，`begin_refresh` 会抢占这
    /// 个预约并立即开始，避免用户操作等待计时器。
    pub(crate) fn schedule_idle_prewarm(&mut self) -> bool {
        if self.installed.is_some() || self.scanning || self.idle_prewarm_scheduled {
            return false;
        }
        self.idle_prewarm_scheduled = true;
        true
    }
}

/// 一个支持打开项目目录的本地 IDE。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InstalledIde {
    pub(crate) label: &'static str,
    /// macOS 上优先使用应用包声明的原始图标，避免系统为透明图标叠加通用底板；
    /// CLI-only 或取图标失败时为空，UI 会使用通用线性图标作为明确的回退。
    pub(crate) icon: Option<Arc<gpui::Image>>,
    launcher: Launcher,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Launcher {
    /// macOS Launch Services 已解析出的 `.app` 绝对路径。启动时复用它，避免再按
    /// 名称走一遍 Launch Services。
    MacApp(PathBuf),
    /// PATH 中可以直接执行的 CLI 名称。
    Command(String),
}

struct IdeDefinition {
    label: &'static str,
    /// 按顺序尝试的 macOS 应用显示名称。名称可以和 `.app` 文件名不同，
    /// `NSWorkspace` 会交给 Launch Services 做解析。
    mac_apps: &'static [&'static str],
    /// 按顺序尝试的命令名；macOS 上作为 app 探测失败后的回退，其他平台是
    /// 唯一探测方式。
    commands: &'static [&'static str],
}

// 这里按常见程度排序；新增编辑器只需要补一条定义，不必改 UI。
const IDE_DEFINITIONS: &[IdeDefinition] = &[
    IdeDefinition {
        label: "Zed",
        mac_apps: &["Zed"],
        commands: &["zed"],
    },
    IdeDefinition {
        label: "Antigravity 2.0",
        mac_apps: &["Antigravity"],
        commands: &["antigravity"],
    },
    IdeDefinition {
        label: "Antigravity IDE",
        mac_apps: &["Antigravity IDE"],
        commands: &[],
    },
    IdeDefinition {
        label: "Cursor",
        mac_apps: &["Cursor"],
        commands: &["cursor"],
    },
    IdeDefinition {
        label: "VS Code",
        mac_apps: &["Visual Studio Code"],
        commands: &["code"],
    },
    IdeDefinition {
        label: "VS Code Insiders",
        mac_apps: &["Visual Studio Code - Insiders"],
        commands: &["code-insiders"],
    },
    IdeDefinition {
        label: "Windsurf",
        mac_apps: &["Windsurf"],
        commands: &["windsurf"],
    },
    IdeDefinition {
        label: "IntelliJ IDEA",
        mac_apps: &["IntelliJ IDEA", "IntelliJ IDEA CE"],
        commands: &["idea"],
    },
    IdeDefinition {
        label: "Android Studio",
        mac_apps: &["Android Studio"],
        commands: &["studio"],
    },
    IdeDefinition {
        label: "Xcode",
        mac_apps: &["Xcode"],
        commands: &["xed"],
    },
    IdeDefinition {
        label: "CLion",
        mac_apps: &["CLion"],
        commands: &["clion"],
    },
    IdeDefinition {
        label: "GoLand",
        mac_apps: &["GoLand"],
        commands: &["goland"],
    },
    IdeDefinition {
        label: "WebStorm",
        mac_apps: &["WebStorm"],
        commands: &["webstorm"],
    },
    IdeDefinition {
        label: "PyCharm",
        mac_apps: &["PyCharm", "PyCharm CE"],
        commands: &["pycharm"],
    },
    IdeDefinition {
        label: "RustRover",
        mac_apps: &["RustRover"],
        commands: &["rustrover"],
    },
    IdeDefinition {
        label: "Rider",
        mac_apps: &["Rider"],
        commands: &["rider"],
    },
    IdeDefinition {
        label: "Sublime Text",
        mac_apps: &["Sublime Text"],
        commands: &["subl"],
    },
    IdeDefinition {
        label: "Nova",
        mac_apps: &["Nova"],
        commands: &["nova"],
    },
    IdeDefinition {
        label: "Fleet",
        mac_apps: &["Fleet"],
        commands: &["fleet"],
    },
    IdeDefinition {
        label: "Lapce",
        mac_apps: &["Lapce"],
        commands: &["lapce"],
    },
];

/// 返回当前机器上可以打开目录的 IDE，顺序稳定且每个 IDE 只出现一次。
pub(crate) fn detect_installed() -> Vec<InstalledIde> {
    IDE_DEFINITIONS
        .iter()
        .filter_map(|definition| {
            #[cfg(target_os = "macos")]
            if let Some(app_path) = definition.mac_apps.iter().find_map(|app| mac_app_path(app)) {
                return Some(InstalledIde {
                    label: definition.label,
                    icon: mac_app_icon_cached(&app_path),
                    launcher: Launcher::MacApp(app_path),
                });
            }

            definition
                .commands
                .iter()
                .find(|command| executable_in_path(command))
                .map(|command| InstalledIde {
                    label: definition.label,
                    icon: None,
                    launcher: Launcher::Command((*command).to_string()),
                })
        })
        .collect()
}

/// 返回当前平台文件管理器在界面中使用的名称。
pub(crate) fn file_manager_label() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "Finder"
    }

    #[cfg(not(target_os = "macos"))]
    {
        "文件管理器"
    }
}

/// 在后台读取系统文件管理器的应用图标。macOS 上把 Finder 和已发现 IDE 一起缓存，
/// 这样头栏 render 只读取内存快照，不会为了画一个按钮访问 Launch Services。
pub(crate) fn detect_file_manager_icon() -> Option<Arc<gpui::Image>> {
    #[cfg(target_os = "macos")]
    {
        mac_app_path("Finder").and_then(|app_path| mac_app_icon_cached(&app_path))
    }

    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// 在系统文件管理器中打开目录。该函数不等待文件管理器退出。
pub(crate) fn open_in_file_manager(path: &Path) -> Result<(), String> {
    if !path.is_dir() {
        return Err(format!("项目目录不存在或不可访问：{}", path.display()));
    }

    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("open");
        command.arg(path);
        spawn_detached(&mut command, file_manager_label())
    }

    #[cfg(target_os = "windows")]
    {
        let mut command = Command::new("explorer");
        command.arg(path);
        return spawn_detached(&mut command, file_manager_label());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut command = Command::new("xdg-open");
        command.arg(path);
        return spawn_detached(&mut command, file_manager_label());
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", unix)))]
    Err("当前平台不支持打开文件管理器".to_string())
}

/// Open a text file in the platform's default editor without routing through
/// the workspace file panel. This is used by standalone settings windows,
/// which have no visible Workspace file panel to reveal an opened document in.
pub(crate) fn open_in_system_text_editor(path: &Path) -> Result<(), String> {
    if !path.is_file() {
        return Err(format!("配置文件不存在或不可访问：{}", path.display()));
    }

    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("open");
        command.arg("-t").arg(path);
        spawn_detached(&mut command, "系统文本编辑器")
    }

    #[cfg(target_os = "windows")]
    {
        let mut command = Command::new("notepad");
        command.arg(path);
        return spawn_detached(&mut command, "系统文本编辑器");
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut command = Command::new("xdg-open");
        command.arg(path);
        return spawn_detached(&mut command, "系统文本编辑器");
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", unix)))]
    Err("当前平台不支持打开文本编辑器".to_string())
}

/// 在指定 IDE 中打开目录。该函数不等待 IDE 退出，只负责把启动请求交给系统。
pub(crate) fn open_in(ide: &InstalledIde, path: &Path) -> Result<(), String> {
    if !path.is_dir() {
        return Err(format!("项目目录不存在或不可访问：{}", path.display()));
    }

    let mut command = match &ide.launcher {
        #[cfg(target_os = "macos")]
        Launcher::MacApp(app_path) => {
            let mut command = Command::new("open");
            // `open -a` 接受 `.app` 的绝对路径；复用发现结果比按显示名再次解析
            // 更稳定，也避免 IDE 改名或多个同名 app 时打开错误目标。
            command.arg("-a").arg(app_path).arg(path);
            command
        }
        Launcher::Command(program) => {
            let mut command = Command::new(program);
            command.arg(path);
            command
        }
        #[cfg(not(target_os = "macos"))]
        Launcher::MacApp(_) => {
            return Err(format!("{} 只能在 macOS 上启动", ide.label));
        }
    };

    spawn_detached(&mut command, ide.label)
}

fn spawn_detached(command: &mut Command, application: &str) -> Result<(), String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("启动 {application} 失败：{error}"))
}

fn executable_in_path(program: &str) -> bool {
    let program_path = Path::new(program);
    if program_path.components().count() > 1 {
        return is_executable(program_path);
    }

    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<PathBuf>>())
        .map(|directory| directory.join(program))
        .any(|candidate| is_executable(&candidate))
}

fn is_executable(path: &Path) -> bool {
    path.is_file() && {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            path.metadata()
                .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        }
        #[cfg(not(unix))]
        {
            true
        }
    }
}

/// 在 Launch Services 中解析应用名，返回可直接用于取图标和启动的绝对路径。
///
/// 这里刻意不用 `open -Ra`：旧实现每次菜单打开都为所有候选编辑器创建子进程并
/// 等待退出。`NSWorkspace::fullPathForApplication:` 走的是同一份系统注册表，且可从
/// 后台线程调用；外围的 `autoreleasepool` 也保证 worker 线程不会积累临时 AppKit
/// 对象。
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

/// Launch Services 按显示名解析时可能把「Antigravity」命中到
/// `Antigravity IDE.app`。菜单要拆成两项，所以解析结果必须和查询名同一
/// 个 `.app` 文件名。
fn mac_app_bundle_matches_name(path: &Path, app: &str) -> bool {
    path.file_stem().and_then(|stem| stem.to_str()) == Some(app)
}

/// 每个已发现的 `.app` 路径只做一次图标转换；后续 render 只复用已经解码好的
/// `gpui::Image`，不再读取应用包或经过 AppKit 转码。
#[cfg(target_os = "macos")]
fn mac_app_icon_cached(app_path: &Path) -> Option<Arc<gpui::Image>> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    static CACHE: OnceLock<Mutex<HashMap<String, Option<Arc<gpui::Image>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let cache_key = app_path.to_string_lossy().into_owned();
    if let Some(icon) = cache.lock().ok()?.get(&cache_key) {
        return icon.clone();
    }

    let icon = mac_app_icon_png(app_path)
        .map(|bytes| Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, bytes)));
    if let Ok(mut entries) = cache.lock() {
        entries.insert(cache_key, icon.clone());
    }
    icon
}

/// 优先读取 `.app` 包内声明的原始 `.icns` 图标。某些应用（例如 IntelliJ IDEA）的
/// 原始图标带透明区域；若直接通过 `NSWorkspace` 取图，macOS 会额外合成一层通用
/// 应用底板，导致菜单中的图标样式与同组 JetBrains 应用不一致。应用包没有可用图标
/// 时才回退到 NSWorkspace。
///
/// `NSBitmapImageRep` 的 PNG 类型值是 AppKit 定义的 `NSPNGFileType = 4`。
#[cfg(target_os = "macos")]
fn mac_app_icon_png(app_path: &Path) -> Option<Vec<u8>> {
    use objc::rc::autoreleasepool;

    autoreleasepool(|| mac_bundle_icon_png(app_path).or_else(|| mac_workspace_icon_png(app_path)))
}

/// 从应用的 Info.plist 读取图标资源名，并限制在 `Contents/Resources` 内，避免把
/// bundle 中任意值当作文件路径。`CFBundleIconName` 是较新的同义键，作为兼容回退。
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

/// 将包内 `.icns` 解码成 NSImage，再转成 GPUI 可加载的 PNG。
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

/// 应用未声明可读的原始图标时，使用系统为该 `.app` 提供的图标作为兼容回退。
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

/// 将任意 NSImage 转为 PNG；调用方负责在 autorelease pool 中持有 image。
#[cfg(target_os = "macos")]
fn mac_ns_image_png(image: *mut objc::runtime::Object) -> Option<Vec<u8>> {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    unsafe {
        if image.is_null() {
            return None;
        }
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
        Some(std::slice::from_raw_parts(bytes, length).to_vec())
    }
}

#[cfg(target_os = "macos")]
unsafe fn ns_string(value: &str) -> Option<*mut objc::runtime::Object> {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    let c_string = std::ffi::CString::new(value).ok()?;
    let string: *mut Object = msg_send![class!(NSString), stringWithUTF8String: c_string.as_ptr()];
    (!string.is_null()).then_some(string)
}

/// 把 `NSString` 的 UTF-8 内容复制成 Rust 路径；必须在 autorelease pool 内完成。
#[cfg(target_os = "macos")]
unsafe fn ns_path(value: *mut objc::runtime::Object) -> Option<PathBuf> {
    unsafe { ns_string_value(value) }.map(PathBuf::from)
}

/// 把 `NSString` 的 UTF-8 内容复制为 Rust 字符串；必须在 autorelease pool 内完成。
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

#[cfg(test)]
mod tests {
    use super::{IDE_DEFINITIONS, IdeCatalog, executable_in_path};
    use std::path::Path;

    #[test]
    fn catalog_keeps_a_ready_snapshot_while_refreshing() {
        let mut catalog = IdeCatalog::default();
        assert!(catalog.begin_refresh());
        assert_eq!(catalog.snapshot().installed, None);
        assert!(catalog.snapshot().scanning);

        catalog.finish_refresh(Vec::new());
        assert_eq!(catalog.snapshot().installed, Some(Vec::new()));
        assert!(!catalog.snapshot().scanning);

        assert!(catalog.begin_refresh());
        assert_eq!(catalog.snapshot().installed, Some(Vec::new()));
        assert!(catalog.snapshot().scanning);
    }

    #[test]
    fn catalog_schedules_idle_prewarm_once_and_click_can_take_over() {
        let mut catalog = IdeCatalog::default();
        assert!(catalog.schedule_idle_prewarm());
        assert!(!catalog.schedule_idle_prewarm());

        // 菜单先被打开时不等 idle timer，直接开始后台发现。
        assert!(catalog.begin_refresh());
        assert!(!catalog.schedule_idle_prewarm());
    }

    #[test]
    fn catalog_deduplicates_concurrent_refreshes() {
        let mut catalog = IdeCatalog::default();
        assert!(catalog.begin_refresh());
        assert!(!catalog.begin_refresh());
    }

    #[test]
    fn antigravity_desktop_and_ide_are_separate_definitions() {
        let two = IDE_DEFINITIONS
            .iter()
            .find(|definition| definition.label == "Antigravity 2.0")
            .expect("Antigravity 2.0");
        let ide = IDE_DEFINITIONS
            .iter()
            .find(|definition| definition.label == "Antigravity IDE")
            .expect("Antigravity IDE");
        assert!(two.mac_apps.contains(&"Antigravity"));
        assert!(!two.mac_apps.iter().any(|name| name.contains("IDE")));
        assert!(ide.mac_apps.contains(&"Antigravity IDE"));
        assert!(super::mac_app_bundle_matches_name(
            Path::new("/Applications/Antigravity.app"),
            "Antigravity"
        ));
        assert!(!super::mac_app_bundle_matches_name(
            Path::new("/Applications/Antigravity IDE.app"),
            "Antigravity"
        ));
        assert!(super::mac_app_bundle_matches_name(
            Path::new("/Applications/Antigravity IDE.app"),
            "Antigravity IDE"
        ));
    }

    #[test]
    fn ide_definitions_have_stable_unique_labels() {
        for (index, definition) in IDE_DEFINITIONS.iter().enumerate() {
            assert!(!definition.label.trim().is_empty());
            assert!(!definition.mac_apps.is_empty() || !definition.commands.is_empty());
            assert!(
                IDE_DEFINITIONS[..index]
                    .iter()
                    .all(|previous| previous.label != definition.label),
                "重复的 IDE 标签：{}",
                definition.label
            );
        }
    }

    #[test]
    fn path_lookup_finds_current_rust_toolchain() {
        assert!(executable_in_path("rustc"));
        assert!(!executable_in_path("smelt-command-that-does-not-exist"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn installed_app_icons_are_non_empty_and_cached() {
        let installed = super::detect_installed();
        let with_icons = installed
            .iter()
            .filter_map(|ide| ide.icon.as_ref().map(|icon| (ide.label, icon)))
            .collect::<Vec<_>>();
        // CI 机器可能没有任何列出的 IDE；有应用时，每个应用图标都必须是可供 GPUI
        // 解码的非空 PNG，并且第二次探测复用同一份图像 ID。
        if with_icons.is_empty() {
            return;
        }
        assert!(
            with_icons.iter().all(|(_, icon)| {
                icon.format == gpui::ImageFormat::Png && !icon.bytes.is_empty()
            })
        );
        let unique_ids = with_icons
            .iter()
            .map(|(_, icon)| icon.id)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(
            unique_ids.len(),
            with_icons.len(),
            "不同已安装 IDE 应返回不同的系统图标数据"
        );

        let second = super::detect_installed();
        for (label, icon) in with_icons {
            let cached = second
                .iter()
                .find(|ide| ide.label == label)
                .and_then(|ide| ide.icon.as_ref())
                .expect("第二次探测应保留已安装应用图标");
            assert_eq!(icon.id, cached.id);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn finder_icon_is_non_empty_and_cached() {
        let Some(icon) = super::detect_file_manager_icon() else {
            return;
        };
        assert_eq!(icon.format, gpui::ImageFormat::Png);
        assert!(!icon.bytes.is_empty());

        let cached = super::detect_file_manager_icon()
            .expect("Finder 图标第一次读取成功后应保留在应用图标缓存中");
        assert_eq!(icon.id, cached.id);
    }
}
