//! 远程桌面已经拆成内置插件 `smelt-desktop`。宿主里只剩还写死的入口和两处例外，等侧栏机器
//! 那一行（`session_list`）改成插件给的菜单项后整个文件删掉：
//! - 侧栏机器那一行的「远程桌面」按钮和机器菜单项调 [`crate::Workspace::open_desktop`]；
//! - 侧栏顶上的入口不给它（`conversations`），首页出口不存它（`workspace_layout`）。

use gpui::{Context, Window};

/// 远程桌面视图的 id。
pub(crate) const DESKTOP_VIEW: &str = smelt_desktop::VIEW;

impl crate::Workspace {
    /// 打开某台机器的远程桌面：放进出口。同一台开着就接着用，换一台就重连。
    pub(crate) fn open_desktop(
        &mut self,
        machine: &crate::RuntimeKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let args = serde_json::json!({ "machine": smelt_plugin_api::machine_to_json(machine) });
        self.open_view(DESKTOP_VIEW, args, window, cx);
    }
}
