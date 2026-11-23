//! 主窗口的路由：开着哪一页、从哪来，各页出口里放着什么。
//!
//! 路由是嵌套的（见 `smelt_plugin_api::page`）：页是外层，换页记来路，页头的关闭退回去；页的
//! 布局里可以摆一个出口，出口里换内容不换页、不记来路，没放东西时显示这页的默认内容。这里只有
//! 路由的状态和规则，不碰 Dock、注册表和窗口；装布局、露出视图由 `workspace_layout` 按它做。

use std::collections::HashMap;

/// 主窗口的路由状态。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Router {
    /// 当前开着的已登记页 id；`None` 表示尚未落位或没有可用的首页布局。
    page: Option<String>,
    /// 来路，最近的在最后。只在内存里，重启回首页。
    back: Vec<String>,
    /// 各页出口里放着的视图；没有的显示默认内容。离开一页再回来还是走时放着的。
    outlets: HashMap<String, String>,
}

impl Router {
    /// 开着的那页；没有可用页时是 `None`。
    pub(crate) fn page(&self) -> Option<&str> {
        self.page.as_deref()
    }

    /// 这一页出口里放着的视图；放的是默认内容时是 `None`。
    pub(crate) fn outlet(&self, page: &str) -> Option<&str> {
        self.outlets.get(page).map(String::as_str)
    }

    /// 进一页，记下来路。已经在来路上就退回那里，来路不绕圈。已经在这页返回 `false`。
    pub(crate) fn navigate(&mut self, page: String) -> bool {
        if self.page.as_deref() == Some(page.as_str()) {
            return false;
        }
        match self.back.iter().position(|back| back == &page) {
            Some(at) => self.back.truncate(at),
            None => {
                if let Some(prev) = self.page.take() {
                    self.back.push(prev);
                }
            }
        }
        self.page = Some(page);
        true
    }

    /// 来路上最近的一页，从来路上拿掉。调用方进得去就 [`Self::arrive`]，进不去再取下一个。
    pub(crate) fn pop_back(&mut self) -> Option<String> {
        self.back.pop()
    }

    /// 停在这一页，来路不动：退回来路上的一页时用。
    pub(crate) fn arrive(&mut self, page: String) {
        self.page = Some(page);
    }

    /// 落到这一页，来路清空：启动、来路都没了时用。`None` 表示没有可用的首页布局。
    pub(crate) fn land(&mut self, page: Option<String>) {
        self.page = page;
        self.back.clear();
    }

    /// 这一页的出口放这个视图；`None` 放回默认内容。变了返回 `true`。
    pub(crate) fn set_outlet(&mut self, page: &str, view: Option<String>) -> bool {
        let changed = self.outlets.get(page) != view.as_ref();
        match view {
            Some(view) => self.outlets.insert(page.to_owned(), view),
            None => self.outlets.remove(page),
        };
        changed
    }

    /// 页登记变了：进不去的页从来路上拿掉，出口里的东西一起忘掉。开着的那页由调用方处理。
    pub(crate) fn retain_pages(&mut self, usable: impl Fn(&str) -> bool) {
        self.back.retain(|page| usable(page));
        let current = self.page.clone();
        self.outlets
            .retain(|page, _| current.as_deref() == Some(page.as_str()) || usable(page));
    }

    /// 视图登记变了：哪页出口里放着的视图不能再放了（撤下了，按实例建的那份放掉了）就放回
    /// 默认内容。变了返回 `true`。
    pub(crate) fn retain_views(&mut self, keep: impl Fn(&str, &str) -> bool) -> bool {
        let before = self.outlets.len();
        self.outlets.retain(|page, view| keep(page, view));
        self.outlets.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn landed(id: &str) -> Router {
        let mut router = Router::default();
        router.land(Some(id.to_string()));
        router
    }

    #[test]
    fn navigating_records_where_it_came_from() {
        let mut router = landed("work");
        assert!(!router.navigate("work".into()), "已经在这页");
        assert_eq!(router.pop_back(), None);

        assert!(router.navigate("settings".into()));
        assert_eq!(router.page(), Some("settings"));
        assert_eq!(router.pop_back().as_deref(), Some("work"));
        assert_eq!(router.pop_back(), None);
    }

    #[test]
    fn navigating_from_empty_does_not_record_empty_back() {
        let mut router = Router::default();
        assert_eq!(router.page(), None);
        assert!(router.navigate("settings".into()));
        assert_eq!(router.page(), Some("settings"));
        assert_eq!(router.pop_back(), None, "从空态进入设置不记空来路");
    }

    #[test]
    fn going_to_a_page_on_the_way_back_does_not_loop() {
        let mut router = landed("work");
        router.navigate("settings".into());
        router.navigate("update".into());
        router.navigate("work".into());
        assert_eq!(router.page(), Some("work"));
        assert_eq!(router.clone().pop_back(), None, "回到起点，来路清空");

        router.navigate("settings".into());
        router.navigate("update".into());
        router.navigate("settings".into());
        assert_eq!(router.pop_back().as_deref(), Some("work"));
    }

    #[test]
    fn the_outlet_is_kept_per_page_and_is_not_history() {
        let mut router = landed("work");
        assert!(router.set_outlet("work", Some("a/main".into())));
        assert!(!router.set_outlet("work", Some("a/main".into())));
        assert!(router.set_outlet("work", Some("b/main".into())));
        assert_eq!(router.clone().pop_back(), None, "换出口不记来路");

        router.navigate("settings".into());
        assert_eq!(router.outlet("settings"), None);
        let back = router.pop_back().unwrap();
        router.arrive(back);
        assert_eq!(router.outlet("work"), Some("b/main"), "回来还是走时放着的");

        assert!(router.set_outlet("work", None));
        assert_eq!(router.outlet("work"), None);
    }

    #[test]
    fn retired_pages_and_views_are_forgotten() {
        let mut router = landed("work");
        router.set_outlet("work", Some("a/main".into()));
        router.navigate("other".into());
        router.set_outlet("other", Some("b/main".into()));
        router.navigate("settings".into());

        router.retain_pages(|page| page != "other");
        assert_eq!(router.outlet("other"), None);
        assert_eq!(router.pop_back().as_deref(), Some("work"));
        assert_eq!(router.pop_back(), None);

        // 同一个视图放在两页，可以只放掉一页里的（按实例建的那份放掉了）。
        router.set_outlet("other", Some("a/main".into()));
        assert!(router.retain_views(|at, _| at != "other"));
        assert_eq!(router.outlet("work"), Some("a/main"));
        assert_eq!(router.outlet("other"), None);
        assert!(router.retain_views(|_, view| view != "a/main"));
        assert_eq!(router.outlet("work"), None);
        assert!(!router.retain_views(|_, _| true));
    }

    #[test]
    fn landing_forgets_the_way_back_but_not_the_outlets() {
        let mut router = landed("work");
        router.set_outlet("work", Some("a/main".into()));
        router.navigate("settings".into());
        router.land(Some("work".into()));
        assert_eq!(router.pop_back(), None);
        assert_eq!(router.outlet("work"), Some("a/main"));

        router.land(None);
        assert_eq!(router.page(), None);
        assert_eq!(router.pop_back(), None);
    }
}
