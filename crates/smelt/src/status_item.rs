//! macOS 菜单栏右上角常驻图标（`NSStatusItem`）和系统通知桥。
//!
//! 菜单栏图标是宿主给插件的机制（`smelt.app/set-status-menu`）：图标是应用图标，旁边的字、
//! 提示和点开的菜单都由插件给，点一项就执行它带的命令。列什么、怎么排，宿主不管。
//!
//! GPUI 本身完全没有 status item 这个概念，得绕开它直接摸 AppKit。这里还有一道坎：
//! 要**响应点击**，而 AppKit 的按钮/菜单项只认 target-action（一个 Objective-C 对象 +
//! 一个 selector），不认 Rust 闭包或 block，所以必须用 `objc::declare::ClassDecl` 声明
//! 一个最小的 Objective-C 类当 "靶子"。这个类的实例、菜单栏图标、下拉菜单本身都常驻到
//! 进程退出，不需要考虑释放；但下拉菜单里的条目会随插件的设置反复重建，那些临时
//! 对象在交给菜单持有后就显式 release 掉，避免每次重建都攒一份泄漏。

/// 菜单栏与系统通知桥发回 GPUI 主循环的事件。
pub enum StatusItemEvent {
    /// 菜单栏菜单里点了一项，或点了带命令的系统通知：执行它带的命令。
    RunCommand {
        command: String,
        args: serde_json::Value,
    },
    /// 点了一条会话的系统通知。通知只带守护会话 id：主窗口可能已经关了、会话可能重排，
    /// 到点击时再按 id 找当前位置。
    JumpToDaemonSession(String),
    /// 跳到某台机器上某条自动化的一次运行。
    JumpToAutomationRun {
        runtime: crate::RuntimeKey,
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

/// 菜单项点下去要执行的命令，挂在菜单项的 `representedObject` 上（JSON 字符串）。
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(serde::Serialize, serde::Deserialize)]
struct MenuAction {
    command: String,
    args: serde_json::Value,
}

/// 把 PNG 的透明度当形状，涂成 `color`（`0xRRGGBB`）。不是 PNG 是 `None`。
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn tint_png(data: &[u8], color: u32) -> Option<Vec<u8>> {
    let mut rgba = ::image::load_from_memory(data).ok()?.to_rgba8();
    let [_, red, green, blue] = color.to_be_bytes();
    for pixel in rgba.pixels_mut() {
        pixel.0[0] = red;
        pixel.0[1] = green;
        pixel.0[2] = blue;
    }
    let mut encoded = std::io::Cursor::new(Vec::new());
    ::image::DynamicImage::ImageRgba8(rgba)
        .write_to(&mut encoded, ::image::ImageFormat::Png)
        .ok()?;
    Some(encoded.into_inner())
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{MenuAction, StatusItemEvent, system_notifications, tint_png};
    use objc::declare::ClassDecl;
    use objc::runtime::{Class, Object, Sel};
    use objc::{class, msg_send, sel, sel_impl};
    use smelt_plugin_api::{StatusMenu, StatusMenuItem};
    use std::sync::OnceLock;

    /// 应用图标母图（`scripts/make-icon.sh` 产出），直接编进二进制当菜单栏图标用。
    /// 彩色原样显示（不走 `setTemplate:`，那个只吃 alpha 通道，会把带颜色的方形 logo
    /// 拍成纯色剪影）。
    const APP_ICON_PNG: &[u8] = include_bytes!("../../../assets/icon-1024.png");

    /// 菜单项左侧图标统一占用的尺寸。菜单栏是 2x 显示时，13pt 已经足够辨认，
    /// 也能让各项图标使用同一条文字基线。
    const MENU_ICON_SIZE: f64 = 13.0;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NSSize {
        width: f64,
        height: f64,
    }

    /// 点击菜单项只能拿到 Objective-C 层的 self/selector/sender，没法直接闭包捕获；用一个
    /// 全局单例 channel 把事件转发出去，由调用方在 GPUI 事件循环里 drain（这个文件全程原始
    /// AppKit 调用，不能在这里直接摸 GPUI 的 `Context`）。
    static EVENT_TX: OnceLock<smol::channel::Sender<StatusItemEvent>> = OnceLock::new();

    /// 菜单栏图标建好后常驻：按钮、菜单、点击靶子。AppKit 对象只能在主线程摸；指针存成
    /// 整数绕开裸指针不是 `Send`/`Sync` 的限制——本文件所有访问都发生在主线程（AppKit 和
    /// GPUI 事件循环共用同一条主线程），等价于原始指针的正常用法。
    struct Item {
        button: usize,
        menu: usize,
        target: usize,
        has_image: bool,
    }

    static ITEM: OnceLock<Item> = OnceLock::new();

    /// 菜单项的 action：`representedObject` 是 [`set_menu`] 挂上的 [`MenuAction`] JSON。
    extern "C" fn on_run(_this: &Object, _cmd: Sel, sender: *mut Object) {
        let text = unsafe {
            let value: *mut Object = msg_send![sender, representedObject];
            if value.is_null() {
                return;
            }
            let bytes: *const std::os::raw::c_char = msg_send![value, UTF8String];
            if bytes.is_null() {
                return;
            }
            std::ffi::CStr::from_ptr(bytes)
                .to_string_lossy()
                .into_owned()
        };
        let Ok(action) = serde_json::from_str::<MenuAction>(&text) else {
            return;
        };
        if let Some(tx) = EVENT_TX.get() {
            let _ = tx.try_send(StatusItemEvent::RunCommand {
                command: action.command,
                args: action.args,
            });
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
                    sel!(smeltStatusItemRun:),
                    on_run as extern "C" fn(&Object, Sel, *mut Object),
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

    /// PNG → `NSImage`（+1，调用方挂到菜单项上后 release）。数据坏了是 `None`。
    unsafe fn image(data: &[u8], size: f64) -> Option<*mut Object> {
        let ns_data: *mut Object = msg_send![
            class!(NSData),
            dataWithBytes: data.as_ptr() as *const std::ffi::c_void
            length: data.len()
        ];
        let image: *mut Object = msg_send![class!(NSImage), alloc];
        let image: *mut Object = msg_send![image, initWithData: ns_data];
        if image.is_null() {
            return None;
        }
        let _: () = msg_send![image, setSize: NSSize {
            width: size,
            height: size,
        }];
        Some(image)
    }

    /// 第一次设菜单时建菜单栏图标：应用 icon 母图缩到菜单栏尺寸，取不到（理论上不会，PNG
    /// 编进二进制里的，兜底而已）就退化成文字。图标、菜单、点击靶子实例都常驻到进程退出，
    /// 故意不释放。
    fn item() -> &'static Item {
        ITEM.get_or_init(|| unsafe {
            let bar: *mut Object = msg_send![class!(NSStatusBar), systemStatusBar];
            // NSVariableStatusItemLength == -1.0，让系统按内容自适应宽度。
            let item: *mut Object = msg_send![bar, statusItemWithLength: -1.0f64];
            let _: () = msg_send![item, retain]; // 常驻单例：必须自己按住，不然出了这个
            // 作用域就被 autorelease 池收走。

            let button: *mut Object = msg_send![item, button];
            let _: () = msg_send![button, retain];
            // 母图是 1024×1024，菜单栏图标按 20pt 显示（跟系统自带图标的观感尺寸对齐），
            // NSImage 自己插值缩小，不用额外裁剪/预生成小图。
            let has_image = match image(APP_ICON_PNG, 20.0) {
                Some(icon) => {
                    let _: () = msg_send![button, setImage: icon];
                    let _: () = msg_send![icon, release];
                    let _: () = msg_send![button, setImagePosition: 2u64]; // NSImageLeft：图标靠左，字跟在右边
                    true
                }
                None => false,
            };

            let target: *mut Object = msg_send![target_class(), new]; // +1，永不 release
            let menu: *mut Object = msg_send![class!(NSMenu), new]; // +1，永不 release
            let _: () = msg_send![item, setMenu: menu]; // 挂了菜单后，点按钮直接弹菜单
            Item {
                button: button as usize,
                menu: menu as usize,
                target: target as usize,
                has_image,
            }
        })
    }

    /// 装好点击事件的出口和系统通知桥。菜单栏图标等第一次设菜单时才建。
    pub fn setup(tx: smol::channel::Sender<StatusItemEvent>) {
        let _ = EVENT_TX.set(tx.clone());
        system_notifications::setup(tx);
    }

    /// 按插件给的设置重画菜单栏图标旁的字、提示和菜单：先清空，逐项建菜单项（命令挂在
    /// `representedObject` 上，点击经 `on_run` 转发）。
    pub fn set_menu(status: &StatusMenu) {
        let item = item();
        unsafe {
            let button = item.button as *mut Object;
            let title = if status.title.is_empty() && !item.has_image {
                "smelt"
            } else {
                status.title.as_str()
            };
            let _: () = msg_send![button, setTitle: nsstring(title)];
            let _: () = msg_send![button, setToolTip: nsstring(&status.tooltip)];

            let menu = item.menu as *mut Object;
            let target = item.target as *mut Object;
            let _: () = msg_send![menu, removeAllItems];
            for entry in &status.items {
                match entry {
                    StatusMenuItem::Separator => {
                        let separator: *mut Object = msg_send![class!(NSMenuItem), separatorItem];
                        let _: () = msg_send![menu, addItem: separator];
                    }
                    StatusMenuItem::Action {
                        label,
                        icon,
                        command,
                        args,
                    } => {
                        let Ok(action) = serde_json::to_string(&MenuAction {
                            command: command.clone(),
                            args: args.clone(),
                        }) else {
                            continue;
                        };
                        let menu_item: *mut Object = msg_send![class!(NSMenuItem), alloc];
                        let menu_item: *mut Object = msg_send![menu_item,
                            initWithTitle: nsstring(label)
                            action: sel!(smeltStatusItemRun:)
                            keyEquivalent: nsstring("")];
                        // 菜单项强引用 representedObject，点击经 on_run 带回。
                        let _: () = msg_send![menu_item, setRepresentedObject: nsstring(&action)];
                        let _: () = msg_send![menu_item, setTarget: target];
                        let icon = icon.as_ref().and_then(|icon| {
                            let tinted = tint_png(&icon.png, icon.color)?;
                            image(&tinted, MENU_ICON_SIZE)
                        });
                        if let Some(icon) = icon {
                            let _: () = msg_send![menu_item, setImage: icon];
                            let _: () = msg_send![icon, release]; // setImage: 会 copy 一份
                        }
                        let _: () = msg_send![menu, addItem: menu_item];
                        let _: () = msg_send![menu_item, release]; // addItem: 会 retain 一份
                    }
                }
            }
        }
    }

    /// 把 app 前置（smelt 只有一扇主窗口，前置整个 app 等价于前置它）。
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
    use super::{StatusItemEvent, SystemNotificationAuthorization, SystemNotificationStatus};
    use smelt_plugin_api::StatusMenu;

    pub fn setup(_tx: smol::channel::Sender<StatusItemEvent>) {}
    pub fn set_menu(_status: &StatusMenu) {}
    pub fn activate_app() {}
    pub fn is_app_active() -> bool {
        true
    }
    pub fn deliver_notification(_session_id: &str, _subtitle: &str, _body: &str) {}
    pub fn deliver_app_notification(_subtitle: &str, _body: &str) {}
    pub fn deliver_plugin_notification(_notify: &smelt_plugin_api::Notify) {}
    pub fn withdraw_plugin_notification(_id: &str) {}
    pub fn notify_error(message: impl AsRef<str>) {
        deliver_app_notification("错误", message.as_ref());
    }
    pub fn notify_success(message: impl AsRef<str>) {
        deliver_app_notification("完成", message.as_ref());
    }
    pub fn deliver_automation_notification(
        _runtime: &crate::RuntimeKey,
        _automation_id: &str,
        _run_id: &str,
        _subtitle: &str,
        _body: &str,
    ) {
    }
    pub fn retain_system_notifications(_session_ids: &[String]) {}
    pub fn retain_automation_notifications(_runtime: &crate::RuntimeKey, _run_ids: &[String]) {}
    pub fn remove_system_notification(_session_id: &str) {}
    pub fn remove_automation_notification(_runtime: &crate::RuntimeKey, _run_id: &str) {}
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
}

pub use imp::{activate_app, is_app_active, set_menu, setup};

#[cfg(target_os = "macos")]
pub use system_notifications::{
    deliver_app_notification, deliver_automation_notification, deliver_notification,
    deliver_plugin_notification, notify_error, notify_success,
    refresh_system_notification_authorization, remove_automation_notification,
    remove_system_notification, retain_automation_notifications, retain_system_notifications,
    sync_system_notifications, system_notification_status, take_system_notification_errors,
    withdraw_plugin_notification,
};

#[cfg(not(target_os = "macos"))]
pub use imp::{
    deliver_app_notification, deliver_automation_notification, deliver_notification,
    deliver_plugin_notification, notify_error, notify_success,
    refresh_system_notification_authorization, remove_automation_notification,
    remove_system_notification, retain_automation_notifications, retain_system_notifications,
    sync_system_notifications, system_notification_status, take_system_notification_errors,
    withdraw_plugin_notification,
};

#[cfg(test)]
mod tests {
    use super::{
        SystemNotificationAuthorization, SystemNotificationStatus,
        system_notification_status_presentation, tint_png,
    };

    /// 上色只换颜色，形状（透明度）不动。
    #[test]
    fn tinting_keeps_the_alpha_mask() {
        let mut source = ::image::RgbaImage::new(2, 1);
        source.put_pixel(0, 0, ::image::Rgba([255, 255, 255, 255]));
        source.put_pixel(1, 0, ::image::Rgba([0, 0, 0, 0]));
        let mut png = std::io::Cursor::new(Vec::new());
        ::image::DynamicImage::ImageRgba8(source)
            .write_to(&mut png, ::image::ImageFormat::Png)
            .unwrap();
        let tinted = tint_png(png.get_ref(), 0x112233).expect("是 PNG");
        let tinted = ::image::load_from_memory(&tinted).unwrap().to_rgba8();
        assert_eq!(tinted.get_pixel(0, 0).0, [0x11, 0x22, 0x33, 255]);
        assert_eq!(tinted.get_pixel(1, 0).0[3], 0);
        assert_eq!(tint_png(b"not a png", 0), None);
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
