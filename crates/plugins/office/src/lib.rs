//! 像素办公室：只读运行时名册和状态，按项目摆角色，经会话插件的公开视图约定跳转。
//! 不读 provider 的私有存档、不持久化镜像、不在宿主增加功能分支。

mod actors;
mod layout;
mod model;
mod motion;
mod scene;
mod view;

use std::rc::Rc;

use gpui::{App, AppContext as _};
use smelt_plugin_api as api;

rust_i18n::i18n!("locales", fallback = "en");

const OWNER: &str = "smelt.office";
const VIEW: &str = "smelt.office/main";

/// 登记独立总览，启动栏按视图注册表自动给入口。
pub fn register(cx: &mut App) {
    let spec = api::ViewSpec {
        name: "main".into(),
        title: api::text!("office.title"),
        icon: api::ui::IconName::LayoutGrid.path(),
        draws_top_bar: false,
        settings: None,
        instances: false,
        fills: Vec::new(),
        build: Rc::new(|_, cx| Rc::new(cx.new(view::Office::new))),
    };
    if let Err(error) = api::register_views(OWNER, vec![spec], cx) {
        eprintln!("[office] 视图登记失败：{error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn registers_a_launchable_view_and_can_be_removed(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            register(cx);
            let view = api::view(VIEW, cx).unwrap();
            assert!(!view.instances && view.settings.is_none());
            assert!(view.fills.is_empty());
            api::unregister_views(OWNER, cx);
            assert!(api::view(VIEW, cx).is_none());
        });
    }
}
