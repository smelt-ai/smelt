//! Dock 图标角标：宿主给插件的机制（`smelt.app/set-dock-badge`），显示什么由插件定。

/// 设置 Dock 图标角标，`None` 清掉。
/// `[[NSApplication sharedApplication] dockTile] setBadgeLabel:]`——跟应用是否在
/// 前台无关，全局唯一，不需要拿具体窗口。
#[cfg(target_os = "macos")]
pub fn set_badge(label: Option<&str>) {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    unsafe {
        let ns_app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        if ns_app.is_null() {
            return;
        }
        let dock_tile: *mut Object = msg_send![ns_app, dockTile];
        if dock_tile.is_null() {
            return;
        }
        let label: *mut Object = match label {
            None => std::ptr::null_mut(), // nil 清空角标
            Some(label) => {
                let Ok(c_string) = std::ffi::CString::new(label) else {
                    return;
                };
                msg_send![class!(NSString), stringWithUTF8String: c_string.as_ptr()]
            }
        };
        let _: () = msg_send![dock_tile, setBadgeLabel: label];
    }
}

#[cfg(not(target_os = "macos"))]
pub fn set_badge(_label: Option<&str>) {}
