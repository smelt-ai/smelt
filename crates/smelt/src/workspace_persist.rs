//! 宿主自己的界面状态：首页出口里放着的视图（见 [`crate::router`]），存在客户端库
//! （`workspace_ui`），每台设备可以不同。各页的布局另存（见 [`crate::workspace_layout`]）；
//! 功能的界面状态归各自的插件，不经这里。

use gpui::App;

use crate::Workspace;

/// 启动时同步读首页出口里放着的视图（客户端库，本地，很快）。没存过、读不了、放的是默认
/// 内容时是 `None`。放得下放不下由打开首页时按注册表定。
pub(crate) fn load_outlet() -> Option<String> {
    match smelt_client_store::client_store().and_then(|store| store.workspace_ui()) {
        Ok(record) => record
            .map(|record| record.outlet)
            .filter(|view| !view.is_empty()),
        Err(error) => {
            eprintln!("[workspace] 读取界面状态失败，使用默认值：{error}");
            None
        }
    }
}

impl Workspace {
    /// 把宿主的界面状态写进客户端库。和上次写的一样就不写。只有一行，和布局存档一样在
    /// 事件里当场写；不从 render 里调。
    pub(crate) fn save_state(&mut self, cx: &App) {
        let outlet = self.persisted_outlet(cx);
        if self.saved_outlet.as_ref() == Some(&outlet) {
            return;
        }
        let record = smelt_client_store::WorkspaceUiRecord {
            outlet: outlet.clone().unwrap_or_default(),
        };
        match smelt_client_store::client_store().and_then(|store| store.put_workspace_ui(&record)) {
            Ok(()) => self.saved_outlet = Some(outlet),
            Err(error) => eprintln!("[workspace] 存界面状态失败：{error}"),
        }
    }
}
