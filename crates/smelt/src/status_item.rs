//! macOS 菜单栏右上角常驻图标（`NSStatusItem`）和系统通知桥。
//!
//! 菜单栏图标是宿主给插件的机制（`smelt_plugin_api::set_status_menu`）：每个插件一个，图标、
//! 旁边的字、提示和点开的菜单都由插件给，点一项就执行它带的命令。列什么、怎么排，宿主不管。
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

/// 允许通知时，用户在系统设置里给这个应用开着的几项。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SystemNotificationDelivery {
    pub banners: bool,
    pub sound: bool,
    pub badge: bool,
}

/// 系统通知桥的可观测状态：插件读授权，日志记积压。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemNotificationStatus {
    pub authorization: SystemNotificationAuthorization,
    /// 上一次从系统读到的授权和各项。回到前台重查时 `authorization` 先变回 `Unknown`，
    /// 给人看的用这份，免得每次切回来提示都闪一下。
    pub last_read: Option<(SystemNotificationAuthorization, SystemNotificationDelivery)>,
    pub pending_count: usize,
    pub last_error: Option<String>,
}

/// 给插件看的系统通知授权（`Host::notification_permission`）。
pub(crate) fn notification_permission() -> smelt_plugin_api::NotificationPermission {
    use smelt_plugin_api::NotificationPermission as Permission;
    if cfg!(not(target_os = "macos")) {
        return Permission::Unsupported;
    }
    permission_from_status(system_notification_status())
}

fn permission_from_status(
    status: SystemNotificationStatus,
) -> smelt_plugin_api::NotificationPermission {
    use smelt_plugin_api::NotificationPermission as Permission;
    let (authorization, delivery) = match (status.authorization, status.last_read) {
        (SystemNotificationAuthorization::Unavailable, _) => {
            return Permission::Unavailable(
                status
                    .last_error
                    .unwrap_or_else(|| rust_i18n::t!("notify.permission_unknown").into_owned()),
            );
        }
        (_, Some(last_read)) => last_read,
        (_, None) => return Permission::Checking,
    };
    match authorization {
        SystemNotificationAuthorization::NotDetermined => Permission::NotRequested,
        SystemNotificationAuthorization::Denied => Permission::Denied,
        SystemNotificationAuthorization::Authorized => Permission::Granted {
            banners: delivery.banners,
            sound: delivery.sound,
            badge: delivery.badge,
        },
        SystemNotificationAuthorization::Unknown | SystemNotificationAuthorization::Unavailable => {
            Permission::Checking
        }
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

/// 菜单栏上一张图的字节：给的是字节就用，给的是资源路径就从应用的资源表读。读不到是 `None`。
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn image_bytes<'a>(
    image: &'a smelt_plugin_api::StatusImage,
    assets: &dyn gpui::AssetSource,
) -> Option<std::borrow::Cow<'a, [u8]>> {
    match image {
        smelt_plugin_api::StatusImage::Png(bytes) => Some(std::borrow::Cow::Borrowed(bytes)),
        smelt_plugin_api::StatusImage::Asset(path) => match assets.load(path) {
            Ok(Some(bytes)) => Some(std::borrow::Cow::Owned(bytes.into_owned())),
            Ok(None) => None,
            Err(error) => {
                eprintln!("[status_item] 读菜单栏图片「{path}」失败：{error}");
                None
            }
        },
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{MenuAction, StatusItemEvent, image_bytes, system_notifications, tint_png};
    use gpui::AssetSource;
    use objc::declare::ClassDecl;
    use objc::runtime::{Class, Object, Sel};
    use objc::{class, msg_send, sel, sel_impl};
    use smelt_plugin_api::{StatusMenu, StatusMenuItem};
    use std::cell::RefCell;
    use std::sync::OnceLock;

    /// 应用图标母图（`scripts/make-icon.sh` 产出），插件没给图标时当菜单栏图标用。
    const APP_ICON_PNG: &[u8] = include_bytes!("../../../assets/icon-1024.png");

    /// 应用图标按 20pt 显示，跟系统自带图标的观感尺寸对齐。
    const APP_ICON_SIZE: f64 = 20.0;

    /// 插件给的模板图按系统菜单栏图标的常规尺寸显示。
    const STATUS_ICON_SIZE: f64 = 18.0;

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

    thread_local! {
        /// 来源 → 它的菜单栏图标（`NSStatusItem`，自己按住的那一份）和上次画的样子。AppKit
        /// 对象只能在主线程摸，同步也只在主线程做，放线程局部。
        static ITEMS: RefCell<Vec<(String, Item)>> = const { RefCell::new(Vec::new()) };
    }

    struct Item {
        status_item: *mut Object,
        drawn: StatusMenu,
    }

    /// 菜单项的 action：`representedObject` 是 [`draw`] 挂上的 [`MenuAction`] JSON。
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

    /// 所有菜单项共用的点击靶子，常驻到进程退出。
    fn target() -> *mut Object {
        static TARGET: OnceLock<usize> = OnceLock::new();
        *TARGET.get_or_init(|| unsafe {
            let target: *mut Object = msg_send![target_class(), new]; // +1，永不 release
            target as usize
        }) as *mut Object
    }

    /// 新建一个菜单栏图标，挂上空菜单。新的排在已有的左边（系统的规矩）。
    fn create() -> *mut Object {
        unsafe {
            let bar: *mut Object = msg_send![class!(NSStatusBar), systemStatusBar];
            // NSVariableStatusItemLength == -1.0，让系统按内容自适应宽度。
            let item: *mut Object = msg_send![bar, statusItemWithLength: -1.0f64];
            // 拿到的是 autorelease 的，不按住出了这个作用域图标就没了；撤掉时 release。
            let _: () = msg_send![item, retain];
            let menu: *mut Object = msg_send![class!(NSMenu), new];
            let _: () = msg_send![item, setMenu: menu]; // 挂了菜单后，点按钮直接弹菜单
            let _: () = msg_send![menu, release]; // setMenu: 会 retain 一份
            let button: *mut Object = msg_send![item, button];
            let _: () = msg_send![button, setImagePosition: 2u64]; // NSImageLeft：图标靠左，字跟在右边
            item
        }
    }

    /// 撤掉一个菜单栏图标。
    fn remove(item: *mut Object) {
        unsafe {
            let bar: *mut Object = msg_send![class!(NSStatusBar), systemStatusBar];
            let _: () = msg_send![bar, removeStatusItem: item];
            let _: () = msg_send![item, release];
        }
    }

    /// 装好点击事件的出口和系统通知桥。菜单栏图标等有插件设了才建（见 [`sync`]）。
    pub fn setup(tx: smol::channel::Sender<StatusItemEvent>) {
        let _ = EVENT_TX.set(tx.clone());
        system_notifications::setup(tx);
    }

    /// 按插件们设的菜单栏图标同步：没了的撤掉，新来的建上，变了的重画，没变的不动。
    pub fn sync(menus: &[(String, StatusMenu)], assets: &dyn AssetSource) {
        ITEMS.with(|items| {
            let mut items = items.borrow_mut();
            items.retain(|(owner, item)| {
                let keep = menus.iter().any(|(wanted, _)| wanted == owner);
                if !keep {
                    remove(item.status_item);
                }
                keep
            });
            for (owner, menu) in menus {
                match items.iter_mut().find(|(existing, _)| existing == owner) {
                    Some((_, item)) if item.drawn == *menu => {}
                    Some((_, item)) => {
                        draw(item.status_item, Some(&item.drawn), menu, assets);
                        item.drawn = menu.clone();
                    }
                    None => {
                        let status_item = create();
                        draw(status_item, None, menu, assets);
                        items.push((
                            owner.clone(),
                            Item {
                                status_item,
                                drawn: menu.clone(),
                            },
                        ));
                    }
                }
            }
        });
    }

    /// 画一个菜单栏图标：图标（变了才换，`drawn` 是上次画的）、旁边的字、提示，菜单先清空
    /// 再逐项建（命令挂在 `representedObject` 上，点击经 `on_run` 转发）。
    fn draw(
        status_item: *mut Object,
        drawn: Option<&StatusMenu>,
        status: &StatusMenu,
        assets: &dyn AssetSource,
    ) {
        unsafe {
            let button: *mut Object = msg_send![status_item, button];
            if drawn.is_none_or(|drawn| drawn.icon != status.icon) {
                // 插件给的图当模板图：只取透明度，颜色由系统按菜单栏深浅定。没给、或给的不是图，
                // 用应用图标，彩色原样显示（模板图会把带颜色的方形 logo 拍成纯色剪影）。
                let custom = status
                    .icon
                    .as_ref()
                    .and_then(|icon| image(&image_bytes(icon, assets)?, STATUS_ICON_SIZE));
                let icon = match custom {
                    Some(icon) => {
                        let _: () = msg_send![icon, setTemplate: objc::runtime::YES];
                        Some(icon)
                    }
                    // 母图是 1024×1024，NSImage 自己插值缩小。
                    None => image(APP_ICON_PNG, APP_ICON_SIZE),
                };
                if let Some(icon) = icon {
                    let _: () = msg_send![button, setImage: icon];
                    let _: () = msg_send![icon, release];
                }
            }
            let has_image: *mut Object = msg_send![button, image];
            let title = if status.title.is_empty() && has_image.is_null() {
                "smelt"
            } else {
                status.title.as_str()
            };
            let _: () = msg_send![button, setTitle: nsstring(title)];
            let _: () = msg_send![button, setToolTip: nsstring(&status.tooltip)];

            let menu: *mut Object = msg_send![status_item, menu];
            let target = target();
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
                            let tinted = tint_png(&image_bytes(&icon.png, assets)?, icon.color)?;
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
    pub fn sync(_menus: &[(String, StatusMenu)], _assets: &dyn gpui::AssetSource) {}
    pub fn activate_app() {}
    pub fn is_app_active() -> bool {
        true
    }
    fn deliver_app_notification(_subtitle: &str, _body: &str) {}
    pub fn deliver_plugin_notification(_notify: &smelt_plugin_api::Notify) {}
    pub fn withdraw_plugin_notification(_id: &str) {}
    pub fn notify_error(message: impl AsRef<str>) {
        deliver_app_notification(&rust_i18n::t!("notify.error"), message.as_ref());
    }
    pub fn notify_success(message: impl AsRef<str>) {
        deliver_app_notification(&rust_i18n::t!("notify.done"), message.as_ref());
    }
    pub fn refresh_system_notification_authorization() {}
    pub fn request_system_notification_authorization() {}
    pub fn system_notification_settings_url() -> Option<String> {
        None
    }
    pub fn sync_system_notifications() {}
    pub fn system_notification_status() -> SystemNotificationStatus {
        SystemNotificationStatus {
            authorization: SystemNotificationAuthorization::Unavailable,
            last_read: None,
            pending_count: 0,
            last_error: Some("system notifications are only available on macOS".to_string()),
        }
    }
    pub fn take_system_notification_errors() -> Vec<String> {
        Vec::new()
    }
}

pub use imp::{activate_app, is_app_active, setup, sync};

#[cfg(target_os = "macos")]
pub use system_notifications::{
    deliver_plugin_notification, notify_error, notify_success,
    refresh_system_notification_authorization, request_system_notification_authorization,
    sync_system_notifications, system_notification_settings_url, system_notification_status,
    take_system_notification_errors, withdraw_plugin_notification,
};

#[cfg(not(target_os = "macos"))]
pub use imp::{
    deliver_plugin_notification, notify_error, notify_success,
    refresh_system_notification_authorization, request_system_notification_authorization,
    sync_system_notifications, system_notification_settings_url, system_notification_status,
    take_system_notification_errors, withdraw_plugin_notification,
};

#[cfg(test)]
mod tests {
    use super::{
        SystemNotificationAuthorization as Authorization, SystemNotificationDelivery,
        SystemNotificationStatus, permission_from_status, tint_png,
    };
    use smelt_plugin_api::NotificationPermission as Permission;

    fn status(
        authorization: Authorization,
        last_read: Option<(Authorization, SystemNotificationDelivery)>,
    ) -> SystemNotificationStatus {
        SystemNotificationStatus {
            authorization,
            last_read,
            pending_count: 0,
            last_error: None,
        }
    }

    /// 回到前台重查时报上一次读到的，不在「读取中」和原状态之间来回闪；各项原样带给插件。
    #[test]
    fn rechecking_keeps_reporting_the_last_read_settings() {
        let delivery = SystemNotificationDelivery {
            banners: true,
            sound: false,
            badge: false,
        };
        let granted = Permission::Granted {
            banners: true,
            sound: false,
            badge: false,
        };
        let read = Some((Authorization::Authorized, delivery));
        assert_eq!(
            permission_from_status(status(Authorization::Authorized, read)),
            granted
        );
        assert_eq!(
            permission_from_status(status(Authorization::Unknown, read)),
            granted
        );
        assert_eq!(
            permission_from_status(status(Authorization::Unknown, None)),
            Permission::Checking
        );
        assert_eq!(
            permission_from_status(status(
                Authorization::Unknown,
                Some((Authorization::Denied, SystemNotificationDelivery::default()))
            )),
            Permission::Denied
        );
        // 申请授权报错：不管上次读到什么，报错误。
        assert!(matches!(
            permission_from_status(status(Authorization::Unavailable, read)),
            Permission::Unavailable(_)
        ));
    }

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
}
