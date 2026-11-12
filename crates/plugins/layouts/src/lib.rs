//! 布局：随程序带的工作台布局。
//!
//! 内置插件，只经 [`smelt_plugin_api`] 登记布局，和第三方布局走同一个注册表、同一套接口。
//! 布局文件随插件编进二进制，不读磁盘。宿主没选过布局时用登记的第一份，这里的默认布局
//! 在宿主启动时最先登记，所以它就是出厂布局。

use gpui::App;
use smelt_plugin_api::{self as api, LayoutSource};

/// 登记布局用的来源名，布局 id 是 `smelt/<名字>`。卸下时用它。
pub const OWNER: &str = "smelt";

/// 随程序带的布局。
pub static LAYOUTS: &[LayoutSource<'static>] = &[LayoutSource {
    name: "default",
    title: "默认",
    state: include_str!("../layouts/default.json"),
}];

/// 宿主启动时调一次，在建工作台之前。再调一次是整组替换，结果相同。
pub fn register(cx: &mut App) {
    api::register_layouts(OWNER, LAYOUTS, cx).expect("内置布局文件不合法");
}

/// 卸下这批布局。在用的布局由宿主换掉。
pub fn unregister(cx: &mut App) {
    api::unregister_layouts(OWNER, cx);
}

#[cfg(test)]
mod tests {
    use api::serde_json::{self, Value};
    use gpui::TestAppContext;

    use super::*;

    /// 一棵面板树里的视图 id，按先后。
    fn views(panel: &Value) -> Vec<String> {
        let children = panel["children"].as_array().cloned().unwrap_or_default();
        if children.is_empty() {
            return vec![panel["panel_name"].as_str().unwrap_or_default().to_string()];
        }
        children.iter().flat_map(views).collect()
    }

    /// 会话列表在左停靠区、文件那组在右停靠区，都开着；舞台在中间。
    #[gpui::test]
    fn the_default_layout_puts_the_side_views_in_open_side_docks(cx: &mut TestAppContext) {
        cx.update(|cx| {
            register(cx);
            // 再登记一次是替换，结果相同。
            register(cx);
            let ids: Vec<String> = api::layouts(cx)
                .into_iter()
                .map(|info| info.id.to_string())
                .collect();
            assert_eq!(ids, ["smelt/default"]);
            let state = api::layout("smelt/default", cx).expect("登记上了");
            let state = serde_json::to_value(&state).unwrap();
            assert_eq!(views(&state["center"]), ["stage"]);
            assert_eq!(state["left_dock"]["open"], true);
            assert_eq!(views(&state["left_dock"]["panel"]), ["sessions"]);
            assert_eq!(state["right_dock"]["open"], true);
            assert_eq!(
                views(&state["right_dock"]["panel"]),
                ["files", "git", "history", "com.smelt.skills"]
            );
            assert!(state["bottom_dock"].is_null());
            unregister(cx);
            assert!(api::layouts(cx).is_empty());
        });
    }
}
