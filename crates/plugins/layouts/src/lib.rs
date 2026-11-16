//! 布局：随程序带的工作台——首页，和它的布局。
//!
//! 内置插件，只经 [`smelt_plugin_api`] 登记页和布局，和第三方走同一个注册表、同一套接口。
//! 布局文件随插件编进二进制，不读磁盘。宿主没选过布局时用给首页登记的第一份，这里的
//! 布局在宿主启动时最先登记，所以它就是出厂布局。

use gpui::App;
use smelt_plugin_api::{self as api, LayoutSource, PageSource};

/// 登记页和布局用的来源名，id 是 `smelt.layouts/<名字>`。卸下时用它。
pub const OWNER: &str = "smelt.layouts";

/// 工作台这一页的 id。
pub const WORKBENCH: &str = "smelt.layouts/workbench";

/// 随程序带的页：工作台，启动时进的就是它。中间是出口，默认放会话舞台。
pub static PAGES: &[PageSource<'static>] = &[PageSource {
    name: "workbench",
    title: "工作台",
    home: true,
    index: Some("smelt.sessions/stage"),
}];

/// 随程序带的布局。来源从 `smelt` 改成 `smelt.layouts` 时 id 跟着变了，旧存档对不上就不装。
pub static LAYOUTS: &[LayoutSource<'static>] = &[LayoutSource {
    page: WORKBENCH,
    name: "standard",
    title: "默认",
    state: include_str!("../layouts/standard.json"),
}];

/// 宿主启动时调一次，在建工作台之前。再调一次是整组替换，结果相同。
pub fn register(cx: &mut App) {
    api::register_pages(OWNER, PAGES, cx).expect("内置页不合法");
    api::register_layouts(OWNER, LAYOUTS, cx).expect("内置布局文件不合法");
}

/// 卸下这页和这批布局。正开着这页时宿主换到别处。
pub fn unregister(cx: &mut App) {
    api::unregister_layouts(OWNER, cx);
    api::unregister_pages(OWNER, cx);
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

    /// 会话列表在左停靠区、文件那组在右停靠区，都开着；出口在中间，默认放舞台。
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
            assert_eq!(ids, ["smelt.layouts/standard"]);
            assert_eq!(
                api::home_page(cx)
                    .map(|page| page.id.to_string())
                    .as_deref(),
                Some(WORKBENCH)
            );
            assert!(api::layouts(cx).iter().all(|info| info.page == WORKBENCH));
            let state = api::layout("smelt.layouts/standard", cx).expect("登记上了");
            let state = serde_json::to_value(&state).unwrap();
            assert_eq!(views(&state["center"]), [api::OUTLET_VIEW]);
            assert_eq!(
                api::home_page(cx).and_then(|page| page.index).as_deref(),
                Some("smelt.sessions/stage")
            );
            assert_eq!(state["left_dock"]["open"], true);
            assert_eq!(views(&state["left_dock"]["panel"]), ["smelt.sessions/list"]);
            assert_eq!(state["right_dock"]["open"], true);
            assert_eq!(
                views(&state["right_dock"]["panel"]),
                [
                    "smelt.files/main",
                    "smelt.git/main",
                    "smelt.history/main",
                    "com.smelt.skills/main"
                ]
            );
            assert!(state["bottom_dock"].is_null());
            unregister(cx);
            assert!(api::layouts(cx).is_empty());
            assert!(api::pages(cx).is_empty());
        });
    }
}
