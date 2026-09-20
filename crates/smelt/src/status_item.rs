//! macOS 菜单栏右上角常驻图标（`NSStatusItem`）：显示待关注和运行中的会话数字，
//! 点开是一个下拉菜单——按状态优先级列出所有会话（状态点 + 名字 + 状态文字），点某一项跳过去；
//! 菜单底部固定一项「打开 smelt 主窗口」，对应原来「点图标就唤出/前置窗口」的行为。
//!
//! GPUI 本身完全没有 status item 这个概念，得绕开它直接摸 AppKit。这里还有一道坎：
//! 要**响应点击**，而 AppKit 的按钮/菜单项只认 target-action（一个 Objective-C 对象 +
//! 一个 selector），不认 Rust 闭包或 block，所以必须用 `objc::declare::ClassDecl` 声明
//! 一个最小的 Objective-C 类当 "靶子"。这个类的实例、菜单栏图标、下拉菜单本身都常驻到
//! 进程退出，不需要考虑释放；但下拉菜单里的会话条目会随会话状态变化反复重建，那些临时
//! 对象在交给菜单持有后就显式 release 掉，避免每次重建都攒一份泄漏。

/// 菜单栏与系统通知桥发回 GPUI 主循环的事件。
pub enum StatusItemEvent {
    ActivateMain,
    JumpToSession(usize),
    JumpToDaemonSession(String),
    JumpToAutomationRun {
        automation_id: String,
        run_id: String,
    },
    /// UserNotifications 的异步权限或投递回执已更新；主线程应继续推进队列。
    SystemNotificationStateChanged,
}

/// 当前系统通知授权状态。`Unknown` 表示正在读取系统设置，`Unavailable` 表示
/// UserNotifications 返回了错误；后者会在应用再次激活时重查。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SystemNotificationAuthorization {
    #[default]
    Unknown,
    NotDetermined,
    Denied,
    Authorized,
    Unavailable,
}

/// 系统通知桥的可观测状态，供设置页或诊断 UI 使用。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemNotificationStatus {
    pub authorization: SystemNotificationAuthorization,
    pub pending_count: usize,
    pub last_error: Option<String>,
}

pub(crate) const SYSTEM_NOTIFICATION_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.Notifications-Settings.extension";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SystemNotificationStatusPresentation {
    pub text: String,
    pub is_error: bool,
    pub can_open_settings: bool,
}

pub(crate) fn system_notification_status_presentation(
    status: &SystemNotificationStatus,
) -> SystemNotificationStatusPresentation {
    let (mut text, is_error, can_open_settings) = match status.authorization {
        SystemNotificationAuthorization::Unknown => ("正在读取…".to_string(), false, false),
        SystemNotificationAuthorization::NotDetermined => {
            ("尚未请求系统授权".to_string(), false, false)
        }
        SystemNotificationAuthorization::Denied => ("已被 macOS 禁止".to_string(), true, true),
        SystemNotificationAuthorization::Authorized => ("已授权".to_string(), false, false),
        SystemNotificationAuthorization::Unavailable => (
            status
                .last_error
                .clone()
                .unwrap_or_else(|| "暂时无法读取授权状态".to_string()),
            true,
            false,
        ),
    };
    if status.pending_count > 0 {
        text.push_str(&format!(" · {} 条提醒待发送", status.pending_count));
    }
    SystemNotificationStatusPresentation {
        text,
        is_error,
        can_open_settings,
    }
}

#[cfg(target_os = "macos")]
mod system_notifications;

/// 下拉菜单里一个会话条目的渲染数据：应用级 attention/daemon 观察器在状态变化时
/// 把最新会话快照喂给 `update_menu`，本文件负责把它翻译成 AppKit 菜单项。
#[derive(Clone, PartialEq)]
pub struct SessionEntry {
    /// 真实会话下标（`self.sessions` 里的位置）。菜单是按状态优先级排过序的，条目在菜单里
    /// 的位置≠会话下标，所以点击要跳到的目标必须显式带上这个原始下标，不能用菜单位置当 tag。
    pub session_ix: usize,
    pub title: String,
    pub status_text: &'static str,
    pub color: (u8, u8, u8),
    /// 会话的 agent 身份 id（裸终端为 None）。菜单项左侧按它查 logo，取不到
    /// （裸终端，或这一家还没有母图）时使用终端图标。
    ///
    /// 存的是 id 而不是枚举：ACP 表和终端表是两张表，一个只能 ACP 跑的 agent
    /// （dsh）在终端表里根本没有变体，用枚举就得在这里替它硬凑一个。
    pub agent: Option<&'static str>,
}

fn status_title(attention_count: usize, running_count: usize) -> String {
    match (attention_count, running_count) {
        (0, 0) => String::new(),
        (attention, 0) => format!("{attention}@"),
        (0, running) => running.to_string(),
        (attention, running) => format!("{attention}@·{running}"),
    }
}

fn status_tooltip(attention_count: usize, running_count: usize) -> String {
    match (attention_count, running_count) {
        (0, 0) => "smelt".to_string(),
        (attention, 0) => format!("{attention} 个会话需要关注"),
        (0, running) => format!("{running} 个会话运行中"),
        (attention, running) => {
            format!("{attention} 个会话需要关注，{running} 个会话运行中")
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{
        SessionEntry, StatusItemEvent, status_title, status_tooltip, system_notifications,
    };
    use objc::declare::ClassDecl;
    use objc::runtime::{Class, Object, Sel};
    use objc::{class, msg_send, sel, sel_impl};
    use std::sync::{
        OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    /// 应用图标母图（`scripts/make-icon.sh` 产出），直接编进二进制当菜单栏图标用——
    /// 不用 SF Symbol 占位符了，用户要的是自己的 logo。彩色原样显示（不走
    /// `setTemplate:`，那个只吃 alpha 通道，会把带颜色的方形 logo 拍成纯色剪影）。
    const APP_ICON_PNG: &[u8] = include_bytes!("../../../assets/icon-1024.png");

    /// 各家 agent 的菜单栏 logo（`crates/smelt/assets/icons/agent-*.svg` 对应的单色
    /// 母图，`assets/agent-*.png`）。菜单项的状态色由 Smelt 自己决定，不能依赖
    /// `setTemplate:` 的系统白/灰色，因此 update_menu 会按状态色把 alpha 蒙版重新着色。
    ///
    /// 按 agent id 查表，不按枚举序号索引：序号索引要求这张表和某一个枚举一一
    /// 对齐，而 agent 现在有 ACP 与终端两张表，且互不为子集。查不到就退回终端
    /// glyph——少一枚 logo 是能看的降级，越界 panic 不是。
    pub(super) const AGENT_LOGO_PNG: &[(&str, &[u8])] = &[
        ("claude", include_bytes!("../../../assets/agent-claude.png")),
        (
            "copilot",
            include_bytes!("../../../assets/agent-copilot.png"),
        ),
        ("codex", include_bytes!("../../../assets/agent-codex.png")),
        ("grok", include_bytes!("../../../assets/agent-grok.png")),
        (
            "antigravity",
            include_bytes!("../../../assets/agent-antigravity.png"),
        ),
        ("cursor", include_bytes!("../../../assets/agent-cursor.png")),
        (
            "opencode",
            include_bytes!("../../../assets/agent-opencode.png"),
        ),
        ("kiro", include_bytes!("../../../assets/agent-kiro.png")),
        ("pi", include_bytes!("../../../assets/agent-pi.png")),
        ("crush", include_bytes!("../../../assets/agent-crush.png")),
        ("dsh", include_bytes!("../../../assets/agent-dsh.png")),
    ];

    /// 菜单项左侧图标统一占用的尺寸。菜单栏是 2x 显示时，13pt 已经足够辨认，
    /// 也能让 agent logo 和终端 glyph 使用同一条文字基线。
    const SESSION_ICON_SIZE: f64 = 13.0;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NSSize {
        width: f64,
        height: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NSPoint {
        x: f64,
        y: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NSRect {
        origin: NSPoint,
        size: NSSize,
    }

    /// 点击/选中菜单项只能拿到 Objective-C 层的 self/selector/sender，没法直接闭包
    /// 捕获；用一个全局单例 channel 把事件转发出去，由调用方在 GPUI 事件循环里 drain
    /// （这个文件全程原始 AppKit 调用，不能在这里直接摸 GPUI 的 `Context`）。
    static EVENT_TX: OnceLock<smol::channel::Sender<StatusItemEvent>> = OnceLock::new();

    /// AppKit 对象只能在主线程摸，但 `setup()` 建好之后，`update_menu`/`set_counts`
    /// 这些后续调用要复用同一个 NSMenu / NSStatusBarButton / target 实例；把指针存成
    /// 整数绕开裸指针不是 `Send`/`Sync` 的限制——本文件所有访问都发生在主线程
    /// （AppKit 和 GPUI 事件循环共用同一条主线程），等价于原始指针的正常用法。
    static MENU_PTR: OnceLock<usize> = OnceLock::new();
    static BUTTON_PTR: OnceLock<usize> = OnceLock::new();
    static TARGET_PTR: OnceLock<usize> = OnceLock::new();
    static BUTTON_HAS_IMAGE: AtomicBool = AtomicBool::new(false);
    static ATTENTION_COUNT: AtomicUsize = AtomicUsize::new(0);
    static RUNNING_COUNT: AtomicUsize = AtomicUsize::new(0);

    fn update_status_title() {
        let Some(&ptr) = BUTTON_PTR.get() else { return };
        let attention_count = ATTENTION_COUNT.load(Ordering::Relaxed);
        let running_count = RUNNING_COUNT.load(Ordering::Relaxed);
        let mut title = status_title(attention_count, running_count);
        if title.is_empty() && !BUTTON_HAS_IMAGE.load(Ordering::Relaxed) {
            title.push_str("smelt");
        }
        let tooltip = status_tooltip(attention_count, running_count);
        unsafe {
            let button = ptr as *mut Object;
            let _: () = msg_send![button, setTitle: nsstring(&title)];
            let _: () = msg_send![button, setToolTip: nsstring(&tooltip)];
        }
    }

    extern "C" fn on_activate(_this: &Object, _cmd: Sel, _sender: *mut Object) {
        if let Some(tx) = EVENT_TX.get() {
            let _ = tx.try_send(StatusItemEvent::ActivateMain);
        }
    }

    /// 会话菜单项的 action：菜单项的 `tag` 就是会话下标（`update_menu` 建条目时按序设的）。
    extern "C" fn on_jump(_this: &Object, _cmd: Sel, sender: *mut Object) {
        let tag: i64 = unsafe { msg_send![sender, tag] };
        if tag >= 0
            && let Some(tx) = EVENT_TX.get()
        {
            let _ = tx.try_send(StatusItemEvent::JumpToSession(tag as usize));
        }
    }

    /// 注册（仅一次）并返回菜单栏点击靶子类。
    fn target_class() -> &'static Class {
        static CLASS: OnceLock<&'static Class> = OnceLock::new();
        CLASS.get_or_init(|| {
            let mut decl = ClassDecl::new("SmeltStatusItemTarget", class!(NSObject))
                .expect("SmeltStatusItemTarget 类重复注册");
            unsafe {
                decl.add_method(
                    sel!(smeltStatusItemActivate:),
                    on_activate as extern "C" fn(&Object, Sel, *mut Object),
                );
                decl.add_method(
                    sel!(smeltStatusItemJump:),
                    on_jump as extern "C" fn(&Object, Sel, *mut Object),
                );
            }
            decl.register()
        })
    }

    /// `&str` → 临时 `NSString*`（autorelease，仅供本次调用内当参数用，不外泄）。
    unsafe fn nsstring(s: &str) -> *mut Object {
        let c = std::ffi::CString::new(s).unwrap_or_default();
        msg_send![class!(NSString), stringWithUTF8String: c.as_ptr()]
    }

    /// 生成和左侧会话菜单一致的方框终端图标。这里直接用 AppKit 画 Lucide
    /// `SquareTerminal` 的几条线，避免依赖 GPUI 的运行时 SVG 资源；调用方用完
    /// （挂到菜单项上）后自己 release 这一份。
    unsafe fn terminal_image(color: (u8, u8, u8)) -> *mut Object {
        let size = NSSize {
            width: SESSION_ICON_SIZE,
            height: SESSION_ICON_SIZE,
        };
        let image: *mut Object = msg_send![class!(NSImage), alloc];
        let image: *mut Object = msg_send![image, initWithSize: size];
        let _: () = msg_send![image, lockFocus];
        let ns_color: *mut Object = msg_send![class!(NSColor),
            colorWithSRGBRed: color.0 as f64 / 255.0
            green: color.1 as f64 / 255.0
            blue: color.2 as f64 / 255.0
            alpha: 1.0f64];
        let _: () = msg_send![ns_color, set];
        let scale = SESSION_ICON_SIZE / 24.0;
        let rect = NSRect {
            origin: NSPoint {
                x: 3.0 * scale,
                y: 3.0 * scale,
            },
            size: NSSize {
                width: 18.0 * scale,
                height: 18.0 * scale,
            },
        };
        let frame: *mut Object = msg_send![class!(NSBezierPath),
            bezierPathWithRoundedRect: rect
            xRadius: 2.0 * scale
            yRadius: 2.0 * scale];
        let _: () = msg_send![frame, setLineWidth: 2.0 * scale];
        let _: () = msg_send![frame, stroke];

        // NSImage 的原点在左下，SquareTerminal SVG 的原点在左上，所以 y 轴要翻转。
        let point = |x: f64, y: f64| NSPoint {
            x: x * scale,
            y: (24.0 - y) * scale,
        };
        let prompt: *mut Object = msg_send![class!(NSBezierPath), bezierPath];
        let _: () = msg_send![prompt, moveToPoint: point(7.0, 11.0)];
        let _: () = msg_send![prompt, lineToPoint: point(9.0, 9.0)];
        let _: () = msg_send![prompt, lineToPoint: point(7.0, 7.0)];
        let _: () = msg_send![prompt, setLineWidth: 2.0 * scale];
        let _: () = msg_send![prompt, stroke];

        let underscore: *mut Object = msg_send![class!(NSBezierPath), bezierPath];
        let _: () = msg_send![underscore, moveToPoint: point(11.0, 13.0)];
        let _: () = msg_send![underscore, lineToPoint: point(15.0, 13.0)];
        let _: () = msg_send![underscore, setLineWidth: 2.0 * scale];
        let _: () = msg_send![underscore, stroke];
        let _: () = msg_send![image, unlockFocus];
        image
    }

    fn tint_agent_logo(data: &[u8], color: (u8, u8, u8)) -> Option<Vec<u8>> {
        let mut rgba = ::image::load_from_memory(data).ok()?.to_rgba8();
        for pixel in rgba.pixels_mut() {
            pixel.0[0] = color.0;
            pixel.0[1] = color.1;
            pixel.0[2] = color.2;
        }
        let mut encoded = std::io::Cursor::new(Vec::new());
        ::image::DynamicImage::ImageRgba8(rgba)
            .write_to(&mut encoded, ::image::ImageFormat::Png)
            .ok()?;
        Some(encoded.into_inner())
    }

    /// 生成一枚按会话状态着色的 agent 菜单栏 logo。与 `terminal_image` 一样：返回的
    /// NSImage 归调用方，挂到菜单项上后由调用方 release 这一份。这一家没有母图
    /// 时返回 None，由调用方退回终端 glyph。
    unsafe fn agent_logo_image(agent: &str, color: (u8, u8, u8)) -> Option<*mut Object> {
        let data = AGENT_LOGO_PNG
            .iter()
            .find(|(id, _)| *id == agent)
            .map(|(_, data)| *data)?;
        let tinted = tint_agent_logo(data, color).unwrap_or_else(|| data.to_vec());
        let ns_data: *mut Object = msg_send![
            class!(NSData),
            dataWithBytes: tinted.as_ptr() as *const std::ffi::c_void
            length: tinted.len()
        ];
        let image: *mut Object = msg_send![class!(NSImage), alloc];
        let image: *mut Object = msg_send![image, initWithData: ns_data];
        let _: () = msg_send![image, setSize: NSSize {
            width: SESSION_ICON_SIZE,
            height: SESSION_ICON_SIZE,
        }];
        Some(image)
    }

    /// 建菜单栏图标 + 空下拉菜单：应用 icon 母图缩到菜单栏尺寸，取不到（理论上不会，
    /// PNG 编进二进制里的，兜底而已）就退化成文字。菜单内容留给 `update_menu` 按会话
    /// 状态填充。图标、菜单、点击靶子实例都常驻到进程退出，故意不释放。
    pub fn setup(tx: smol::channel::Sender<StatusItemEvent>) {
        let _ = EVENT_TX.set(tx.clone());
        unsafe {
            let bar: *mut Object = msg_send![class!(NSStatusBar), systemStatusBar];
            // NSVariableStatusItemLength == -1.0，让系统按内容自适应宽度。
            let item: *mut Object = msg_send![bar, statusItemWithLength: -1.0f64];
            let _: () = msg_send![item, retain]; // 常驻单例：必须自己按住，不然出了这个
            // 作用域就被 autorelease 池收走。

            let button: *mut Object = msg_send![item, button];
            let _: () = msg_send![button, retain];
            let _ = BUTTON_PTR.set(button as usize);

            let data: *mut Object = msg_send![
                class!(NSData),
                dataWithBytes: APP_ICON_PNG.as_ptr() as *const std::ffi::c_void
                length: APP_ICON_PNG.len()
            ];
            let image: *mut Object = msg_send![class!(NSImage), alloc];
            let image: *mut Object = msg_send![image, initWithData: data];
            if !image.is_null() {
                // 母图是 1024×1024，菜单栏图标按 20pt 显示（跟系统自带图标的观感尺寸
                // 对齐），NSImage 自己插值缩小，不用额外裁剪/预生成小图。
                let _: () = msg_send![image, setSize: NSSize { width: 20.0, height: 20.0 }];
                let _: () = msg_send![button, setImage: image];
                let _: () = msg_send![button, setImagePosition: 2u64]; // NSImageLeft：图标靠左，角标数字（若有）跟在右边
                BUTTON_HAS_IMAGE.store(true, Ordering::Relaxed);
            } else {
                let _: () = msg_send![button, setTitle: nsstring("smelt")];
            }
            update_status_title();

            let target: *mut Object = msg_send![target_class(), new]; // +1，永不 release
            let _ = TARGET_PTR.set(target as usize);

            let menu: *mut Object = msg_send![class!(NSMenu), new]; // +1，永不 release
            let _ = MENU_PTR.set(menu as usize);
            let _: () = msg_send![item, setMenu: menu]; // 挂了菜单后，点按钮直接弹菜单，不再走 button 的 target/action
        }
        system_notifications::setup(tx);
    }

    /// Attention observer 可能在工作区不可用时独立到达，因此允许只更新 `@` 前的计数，
    /// 并保留已有运行数。
    pub fn set_attention_count(attention_count: usize) {
        ATTENTION_COUNT.store(attention_count, Ordering::Relaxed);
        update_status_title();
    }

    /// 同步完整状态栏计数：`N@·M` 中 N 是待关注会话数，M 是运行中会话数。
    /// 任一项为 0 时省略该项及分隔点，两项都为 0 时只保留应用图标。
    pub fn set_counts(attention_count: usize, running_count: usize) {
        ATTENTION_COUNT.store(attention_count, Ordering::Relaxed);
        RUNNING_COUNT.store(running_count, Ordering::Relaxed);
        update_status_title();
    }

    /// 按会话快照重建下拉菜单：先清空，逐个会话建一个由 agent logo（裸终端为方框终端
    /// 图标）、标题和状态文字组成的菜单项（`tag` 记会话下标，点击经 `on_jump` 转发），最后加
    /// 一条分隔线 + 固定的「打开 smelt 主窗口」项。
    ///
    /// 只在会话快照真的变化时被调用（见 main.rs），不是每帧都建。
    pub fn update_menu(entries: &[SessionEntry]) {
        let (Some(&menu_ptr), Some(&target_ptr)) = (MENU_PTR.get(), TARGET_PTR.get()) else {
            return;
        };
        unsafe {
            let menu = menu_ptr as *mut Object;
            let target = target_ptr as *mut Object;
            let _: () = msg_send![menu, removeAllItems];

            for entry in entries.iter() {
                let title = format!("{} — {}", entry.title, entry.status_text);
                let item: *mut Object = msg_send![class!(NSMenuItem), alloc];
                let item: *mut Object = msg_send![item,
                    initWithTitle: nsstring(&title)
                    action: sel!(smeltStatusItemJump:)
                    keyEquivalent: nsstring("")];
                // tag 记真实会话下标（不是菜单位置——菜单排过序），点击经 on_jump 原样带回。
                let _: () = msg_send![item, setTag: entry.session_ix as i64];
                let _: () = msg_send![item, setTarget: target];
                // 左侧图标：有 agent 身份用按状态着色的 logo，裸终端使用同一套
                // SquareTerminal glyph；两者都占 SESSION_ICON_SIZE，确保文字对齐。
                let icon = entry
                    .agent
                    .and_then(|agent| agent_logo_image(agent, entry.color))
                    .unwrap_or_else(|| terminal_image(entry.color));
                let _: () = msg_send![item, setImage: icon];
                let _: () = msg_send![icon, release]; // setImage: 会 copy 一份，这份原件用不着了
                let _: () = msg_send![menu, addItem: item];
                let _: () = msg_send![item, release]; // addItem: 会 retain 一份，这份原件用不着了
            }

            if !entries.is_empty() {
                let sep: *mut Object = msg_send![class!(NSMenuItem), separatorItem];
                let _: () = msg_send![menu, addItem: sep];
            }

            let open_item: *mut Object = msg_send![class!(NSMenuItem), alloc];
            let open_item: *mut Object = msg_send![open_item,
                initWithTitle: nsstring("打开 smelt 主窗口")
                action: sel!(smeltStatusItemActivate:)
                keyEquivalent: nsstring("")];
            let _: () = msg_send![open_item, setTarget: target];
            let _: () = msg_send![menu, addItem: open_item];
            let _: () = msg_send![open_item, release];
        }
    }

    /// 点「打开 smelt 主窗口」时若主窗口已经活着：把 app 前置（smelt 目前只有一扇主
    /// 窗口，前置整个 app 等价于前置它，不需要单独找出那扇窗口）。
    pub fn activate_app() {
        unsafe {
            let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![app, activateIgnoringOtherApps: objc::runtime::YES];
        }
    }

    /// 应用是否真的处于 macOS 前台。
    ///
    /// 不能用 GPUI 的 `Window::is_window_active()` 代替：原生全屏窗口切到其它
    /// Space 后仍可能保留 key-window 状态，但 `NSApplication.isActive` 会如实变为
    /// false。是否抑制系统通知必须以应用级事实为准，不能用窗口 key 状态代替。
    pub fn is_app_active() -> bool {
        unsafe {
            let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
            let active: objc::runtime::BOOL = msg_send![app, isActive];
            active == objc::runtime::YES
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::{
        SessionEntry, StatusItemEvent, SystemNotificationAuthorization, SystemNotificationStatus,
    };

    pub fn setup(_tx: smol::channel::Sender<StatusItemEvent>) {}
    pub fn activate_app() {}
    pub fn is_app_active() -> bool {
        true
    }
    pub fn deliver_notification(_session_id: &str, _subtitle: &str, _body: &str) {}
    pub fn deliver_app_notification(_subtitle: &str, _body: &str) {}
    pub fn notify_error(message: impl AsRef<str>) {
        deliver_app_notification("错误", message.as_ref());
    }
    pub fn notify_success(message: impl AsRef<str>) {
        deliver_app_notification("完成", message.as_ref());
    }
    pub fn notify_info(message: impl AsRef<str>) {
        deliver_app_notification("提示", message.as_ref());
    }
    pub fn deliver_automation_notification(
        _automation_id: &str,
        _run_id: &str,
        _subtitle: &str,
        _body: &str,
    ) {
    }
    pub fn retain_system_notifications(_session_ids: &[String]) {}
    pub fn retain_automation_notifications(_run_ids: &[String]) {}
    pub fn remove_system_notification(_session_id: &str) {}
    pub fn remove_automation_notification(_run_id: &str) {}
    pub fn refresh_system_notification_authorization() {}
    pub fn sync_system_notifications() {}
    pub fn system_notification_status() -> SystemNotificationStatus {
        SystemNotificationStatus {
            authorization: SystemNotificationAuthorization::Unavailable,
            pending_count: 0,
            last_error: Some("system notifications are only available on macOS".to_string()),
        }
    }
    pub fn take_system_notification_errors() -> Vec<String> {
        Vec::new()
    }
    pub fn set_attention_count(_attention_count: usize) {}
    pub fn set_counts(_attention_count: usize, _running_count: usize) {}
    pub fn update_menu(_entries: &[SessionEntry]) {}
}

pub use imp::{activate_app, is_app_active, set_attention_count, set_counts, setup, update_menu};

#[cfg(target_os = "macos")]
pub use system_notifications::{
    deliver_automation_notification, deliver_notification, notify_error, notify_info,
    notify_success, refresh_system_notification_authorization, remove_automation_notification,
    remove_system_notification, retain_automation_notifications, retain_system_notifications,
    sync_system_notifications, system_notification_status, take_system_notification_errors,
};

#[cfg(not(target_os = "macos"))]
pub use imp::{
    deliver_automation_notification, deliver_notification, notify_error, notify_info,
    notify_success, refresh_system_notification_authorization, remove_automation_notification,
    remove_system_notification, retain_automation_notifications, retain_system_notifications,
    sync_system_notifications, system_notification_status, take_system_notification_errors,
};

#[cfg(test)]
mod tests {
    use super::{
        SystemNotificationAuthorization, SystemNotificationStatus, status_title, status_tooltip,
        system_notification_status_presentation,
    };

    /// 每一家 agent 都要有菜单栏母图。
    ///
    /// 查表是按 id 的，查不到就静默退回终端方块——那是个「图标不对」的 bug，
    /// 不会崩、也不会有人在 code review 里看出来，所以用不变量把它变成红测试。
    /// 两张表都要覆盖：一家 agent 可能只走 ACP（dsh）或只走终端（antigravity）。
    #[cfg(target_os = "macos")]
    #[test]
    fn every_agent_has_a_status_menu_logo() {
        use smelt_core::agent_kind::{ConversationAgentKind, TerminalAgentKind};
        let ids: Vec<&str> = super::imp::AGENT_LOGO_PNG
            .iter()
            .map(|(id, _)| *id)
            .collect();
        for kind in TerminalAgentKind::ALL {
            assert!(
                ids.contains(&kind.id()),
                "终端 agent {} 没有菜单栏母图",
                kind.id()
            );
        }
        for kind in ConversationAgentKind::ALL {
            assert!(
                ids.contains(&kind.id()),
                "ACP agent {} 没有菜单栏母图",
                kind.id()
            );
        }
    }

    #[test]
    fn status_label_matches_the_compact_attention_and_running_format() {
        assert_eq!(status_title(0, 0), "");
        assert_eq!(status_title(5, 0), "5@");
        assert_eq!(status_title(0, 660), "660");
        assert_eq!(status_title(5, 660), "5@·660");
        assert_eq!(status_tooltip(5, 660), "5 个会话需要关注，660 个会话运行中");
    }

    #[test]
    fn notification_permission_presentation_exposes_denial_and_pending_delivery() {
        let denied = system_notification_status_presentation(&SystemNotificationStatus {
            authorization: SystemNotificationAuthorization::Denied,
            pending_count: 2,
            last_error: Some("permission denied".into()),
        });
        assert_eq!(denied.text, "已被 macOS 禁止 · 2 条提醒待发送");
        assert!(denied.is_error);
        assert!(denied.can_open_settings);

        let authorized = system_notification_status_presentation(&SystemNotificationStatus {
            authorization: SystemNotificationAuthorization::Authorized,
            pending_count: 0,
            last_error: None,
        });
        assert_eq!(authorized.text, "已授权");
        assert!(!authorized.is_error);
        assert!(!authorized.can_open_settings);
    }
}
