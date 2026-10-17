//! 升级时运行时交还给客户端的旧客户端数据。

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LegacyClientState {
    /// 字段名和桌面的外观结构一致（`bg_color`、`theme_name`……）。
    pub appearance: Option<Value>,
    /// `{channel, auto_install}`。
    pub update_settings: Option<Value>,
    /// `{approval, input, success, failure, terminal_bell}`。
    pub notify_prefs: Option<Value>,
    /// 侧栏、当前页、Dock 布局等。可能是整份旧工作区文档，客户端只取它认得的字段。
    pub workspace_ui: Option<Value>,
}
