//! 分屏用 gpui-base 的面板组，分隔条换成和侧栏同一根发丝。
//!
//! 组件库悬停时只在中间亮一小段药丸。侧栏是整根从 `border_dim` 亮到 `text_faint`。
//! 命中和拖拽仍由组件库负责；这里只换画上去的那一根，面板上不再另叠一条边。

use std::rc::Rc;

use gpui::{Axis, ElementId, IntoElement, Styled as _, div, px, rgb};
use gpui_base::{ResizablePanelGroup, ResizeHandleRenderer, ResizeHandleState};

use crate::ui_theme;

pub(crate) fn h_resizable(id: impl Into<ElementId>) -> ResizablePanelGroup {
    gpui_base::h_resizable(id).with_handle_appearance(hairline_handle())
}

pub(crate) fn v_resizable(id: impl Into<ElementId>) -> ResizablePanelGroup {
    gpui_base::v_resizable(id).with_handle_appearance(hairline_handle())
}

/// 整根发丝。静止是 `border_dim`，指针在带上、按下或拖拽时换成 `text_faint`。
///
/// 拖拽一两像素后指针就离开这 9px 的命中带，组件库自己的 hover 会灭。
/// 颜色跟 [`ResizeHandleState`] 走，按下和拖拽也保持亮着。
fn hairline_handle() -> ResizeHandleRenderer {
    Rc::new(|handle, _, _| {
        let color = rgb(if handle.state() == ResizeHandleState::Idle {
            ui_theme::border_dim()
        } else {
            ui_theme::text_faint()
        });
        let line = div().flex_none().bg(color);
        let line = match handle.axis() {
            Axis::Horizontal => line.w(px(1.)).h_full(),
            Axis::Vertical => line.h(px(1.)).w_full(),
        };
        Some(line.into_any_element())
    })
}
