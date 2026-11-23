//! 页的外壳快照：主窗口各页的 `DockArea` 由 `smelt_nsh::MainShell` 承载，[`ShellView`] 是
//! 工作台缓存的只读快照（摆着哪些视图、此刻看得见哪些视图、各视图的 safe area、出口内容、
//! 左右停靠区状态），render 和 safe area 同步只读它。

use std::collections::{HashMap, HashSet};

use smelt_ui::window_bar::SafeArea;

use crate::workspace_frame::WindowCorners;

/// 开着的那页摆着什么、看得见什么。NSH 主壳一变、换页就重算（`Workspace::sync_shell_view`），
/// render 只读它。
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ShellView {
    /// 开着的那页的布局里摆着的视图。
    pub(crate) laid_out: HashSet<String>,
    /// 此刻看得见的视图：所在组显示着的那个（显示着一摞就是摞里的每一个），没被另一组的
    /// 放大挡住，也不在收起的 dock 里。
    pub(crate) shown: HashSet<String>,
    /// 看得见的视图的 safe area。不在这里的全为 0。
    areas: HashMap<String, SafeArea>,
    /// 左、右停靠区的开合状态：`Some(开着没有)`，`None` 表示布局里没有这个停靠区或有组放大着。
    pub(crate) left_dock: Option<bool>,
    pub(crate) right_dock: Option<bool>,
    /// 这页的出口（`smelt_plugin_api::OUTLET_VIEW`）里显示的视图；这页没摆出口是 `None`。
    pub(crate) outlet: Option<String>,
    /// 窗口自己占掉的角：红黄绿按钮（全屏、Windows 下是 0）、窗口圆角。
    corners: WindowCorners,
}

impl ShellView {
    /// 从 NSH 主壳的快照构造 `ShellView`，供 `Workspace` 复用安全区、可见集合和出口路由。
    pub(crate) fn from_nsh(view: &smelt_nsh::ComputedShellView) -> Self {
        Self {
            laid_out: view.laid_out.clone(),
            shown: view.shown.clone(),
            areas: view.areas.iter().map(|(k, v)| (k.clone(), *v)).collect(),
            left_dock: view.left_dock,
            right_dock: view.right_dock,
            outlet: view.outlet.clone(),
            corners: WindowCorners {
                lights: view.corners.lights,
                rounded: view.corners.rounded,
            },
        }
    }

    /// 视图画在哪个面板里：出口里显示着的是出口，别的是它自己。
    pub(crate) fn panel_of<'a>(&self, id: &'a str) -> &'a str {
        if self.outlet.as_deref() == Some(id) {
            smelt_plugin_api::OUTLET_VIEW
        } else {
            id
        }
    }

    /// 视图的 safe area。别的视图（在标签条下面、没摆着、藏着）全为 0。只读缓存。
    pub(crate) fn safe_area_of(&self, id: &str) -> SafeArea {
        self.areas
            .get(self.panel_of(id))
            .copied()
            .unwrap_or_default()
    }
}
