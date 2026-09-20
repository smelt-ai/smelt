//! Dock 图标角标：统计统一 AttentionStore 中「未读或仍需行动」的会话数。普通完成
//! 看过后清除；审批/输入/失败即使看过，也保留到 agent 真正继续。

/// 设置 Dock 图标角标：`count == 0` 清空角标，否则显示数字。
/// `[[NSApplication sharedApplication] dockTile] setBadgeLabel:]`——跟应用是否在
/// 前台无关，全局唯一，不需要拿具体窗口。
#[cfg(target_os = "macos")]
pub fn set_badge(count: usize) {
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
        let label: *mut Object = if count == 0 {
            std::ptr::null_mut() // nil 清空角标
        } else {
            let Ok(c_string) = std::ffi::CString::new(count.to_string()) else {
                return;
            };
            msg_send![class!(NSString), stringWithUTF8String: c_string.as_ptr()]
        };
        let _: () = msg_send![dock_tile, setBadgeLabel: label];
    }
}

#[cfg(not(target_os = "macos"))]
pub fn set_badge(_count: usize) {}
