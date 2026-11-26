//! 宿主自己画在主窗口里的层：调试 HUD。宿主对话框由 NSH 主壳（`MainShell`）承载。

use gpui::{AnyElement, App, Context, IntoElement, ParentElement, Styled, Window, div};

use super::*;

impl Workspace {
    /// 主窗口里开着宿主的遮罩层（宿主对话框）。原生网页视图会盖住这些层，开着时
    /// 插件里的网页让开。
    pub(crate) fn main_overlay_open(&self) -> bool {
        self.show_quit_confirm || self.dir_picker.is_some()
    }

    /// 宿主自己的层，叠在主窗口内容上面。
    pub(crate) fn render_overlay_layers(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .absolute()
            .inset_0()
            .children(self.debug_hud.then(|| debug_hud(window, cx)))
            .into_any_element()
    }
}

/// 调试 HUD（Cmd+Shift+F）：gpui-fps 的帧耗时、慢帧、掉帧和 GPU/CPU/内存。
/// 让出顶部标题栏，免得盖住窗口按钮。
pub(crate) fn debug_hud(window: &mut Window, cx: &mut App) -> Div {
    div()
        .absolute()
        .top(px(40.))
        .left_0()
        .right_0()
        .bottom_0()
        .child(gpui_fps::fps_monitor(window, cx))
}
