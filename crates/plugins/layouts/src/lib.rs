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

/// 随程序带的页：工作台，启动时进的就是它。中间是出口，默认放选中的那段会话。
pub static PAGES: &[PageSource<'static>] = &[PageSource {
    name: "workbench",
    title: "工作台",
    home: true,
    // 约定：smelt.sessions 的会话视图（`SESSION`）。
    index: Some("smelt.sessions/session"),
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

    /// 左停靠区是一摞，从上到下：新对话和入口、会话列表、底下一条左边设置右边额度；文件
    /// 那组在右停靠区，都开着；出口在中间，默认放舞台。
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
                Some("smelt.sessions/session")
            );
            assert_eq!(state["left_dock"]["open"], true);
            let left = api::layout("smelt.layouts/standard", cx)
                .and_then(|state| state.left_dock)
                .expect("有左停靠区");
            let side = &left.panel().children;
            assert_eq!(side.len(), 1, "侧栏整个是一摞");
            let stack = api::Stack::of(&side[0]).expect("是一摞").expect("写得对");
            assert_eq!(
                stack.views(),
                [
                    "smelt.launcher/nav",
                    "smelt.sessions/list",
                    "smelt.launcher/settings",
                    "smelt.agents/quota"
                ]
            );
            // 上下排：列表吃剩下的高度，入口和底下那条按内容定高。底下那条里设置按钮按内容
            // 定宽，额度吃剩下的宽度、窄了在里面折行。
            assert_eq!(stack.axis, api::StackAxis::Vertical);
            let fits: Vec<bool> = stack.items.iter().map(|item| item.fit).collect();
            assert_eq!(fits, [true, false, true]);
            let api::StackContent::Stack(bottom) = &stack.items[2].content else {
                panic!("底下那条是横排的一摞");
            };
            assert_eq!(bottom.axis, api::StackAxis::Horizontal);
            assert!(bottom.items[0].fit && !bottom.items[1].fit);
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
