//! 图标按钮的悬停文案就是动作名时，同时写成无障碍名称。
//!
//! tooltip 只是提示，不会成为读屏名称。没有 `.label()` 的按钮因此没有名字。
//! 说明性的长提示不要用这个方法，它会把整段提示当成名称。

use gpui::SharedString;

use crate::button::Button;

/// 用同一句短动作名设置无障碍名称和悬停提示。
pub trait IconAction {
    fn icon_action(self, name: impl Into<SharedString>) -> Self;
}

impl IconAction for Button {
    fn icon_action(self, name: impl Into<SharedString>) -> Self {
        let name = name.into();
        self.accessibility_label(name.clone()).tooltip(name)
    }
}
