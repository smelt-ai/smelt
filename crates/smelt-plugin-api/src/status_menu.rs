//! macOS 菜单栏右上角的图标：每个来源一个，各管各的字、提示和点开的菜单，互不覆盖。
//!
//! 来源设了才有它的图标，设成 `None`、插件卸下就撤掉；宿主按 [`status_menus`] 画，不管
//! 里面列什么。只有 macOS 有，别的系统登记了也不显示。

use gpui::{App, Global, Subscription};
use serde::{Deserialize, Serialize};

/// 一个来源的菜单栏图标。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusMenu {
    /// 图标：只取透明度当形状，颜色跟着菜单栏的深浅走。不给、读不到用应用图标。
    #[serde(default)]
    pub icon: Option<StatusImage>,
    /// 图标右边的字；空的只显示图标。
    #[serde(default)]
    pub title: String,
    /// 鼠标停在图标上时的提示。
    #[serde(default)]
    pub tooltip: String,
    /// 点开的菜单，从上往下。
    #[serde(default)]
    pub items: Vec<StatusMenuItem>,
}

/// 菜单里的一项。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StatusMenuItem {
    /// 点了执行 `command`（带 `args`），失败由宿主报给用户。
    Action {
        label: String,
        #[serde(default)]
        icon: Option<StatusIcon>,
        command: String,
        #[serde(default)]
        args: serde_json::Value,
    },
    Separator,
}

/// 菜单项左边的小图标：只取透明度当形状，涂成 `color`。读不到就不画。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusIcon {
    pub png: StatusImage,
    /// `0xRRGGBB`；线上写成 `#rrggbb`（[`crate::parse_hex_color`]），脚本直接给主题色。
    #[serde(with = "crate::color::hex")]
    pub color: u32,
}

/// 菜单栏上的一张 PNG。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StatusImage {
    /// 图的字节，Rust 插件用 `include_bytes!` 编进来的。
    Png(Vec<u8>),
    /// 资源表里的路径：插件目录里的文件经 `asset()` 取的路径，或 Smelt 自带的资源。宿主画的时候
    /// 才读。
    Asset(String),
}

/// 各来源的菜单栏图标，按第一次设的次序。
#[derive(Default)]
struct StatusMenus(Vec<(String, StatusMenu)>);

impl Global for StatusMenus {}

/// 设来源 `owner` 的菜单栏图标，`None` 撤掉。和上次一样什么都不做，宿主不用重画。
pub fn set_status_menu(owner: &str, menu: Option<StatusMenu>, cx: &mut App) {
    let current = cx
        .try_global::<StatusMenus>()
        .and_then(|menus| menus.0.iter().find(|(entry, _)| entry == owner))
        .map(|(_, menu)| menu);
    if current == menu.as_ref() {
        return;
    }
    let menus = &mut cx.default_global::<StatusMenus>().0;
    match menu {
        Some(menu) => match menus.iter_mut().find(|(entry, _)| entry == owner) {
            Some((_, current)) => *current = menu,
            None => menus.push((owner.to_string(), menu)),
        },
        None => menus.retain(|(entry, _)| entry != owner),
    }
}

/// 宿主画菜单栏时读：设了图标的来源和它的图标，按第一次设的次序。
pub fn status_menus(cx: &App) -> Vec<(String, StatusMenu)> {
    cx.try_global::<StatusMenus>()
        .map(|menus| menus.0.clone())
        .unwrap_or_default()
}

/// 宿主用：有来源的菜单栏图标变了就调 `changed`。
pub fn observe_status_menus(cx: &mut App, changed: impl FnMut(&mut App) + 'static) -> Subscription {
    cx.default_global::<StatusMenus>();
    cx.observe_global::<StatusMenus>(changed)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;

    fn titled(title: &str) -> StatusMenu {
        StatusMenu {
            title: title.into(),
            ..StatusMenu::default()
        }
    }

    #[gpui::test]
    fn each_owner_keeps_its_own_menu(cx: &mut gpui::TestAppContext) {
        let changes = Rc::new(Cell::new(0));
        let _subscription = cx.update(|cx| {
            let changes = changes.clone();
            observe_status_menus(cx, move |_| changes.set(changes.get() + 1))
        });
        cx.update(|cx| {
            set_status_menu("a", Some(titled("1")), cx);
            set_status_menu("b", Some(titled("行情")), cx);
            // 后设的不顶掉先设的，改自己的不挪位置。
            set_status_menu("a", Some(titled("2")), cx);
        });
        let titles = |cx: &mut gpui::TestAppContext| {
            cx.read(|cx| {
                status_menus(cx)
                    .into_iter()
                    .map(|(owner, menu)| format!("{owner}:{}", menu.title))
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(titles(cx), ["a:2", "b:行情"]);
        let seen = changes.get();
        assert!(seen > 0);

        // 一样的不算变。
        cx.update(|cx| set_status_menu("b", Some(titled("行情")), cx));
        assert_eq!(changes.get(), seen);

        cx.update(|cx| set_status_menu("a", None, cx));
        assert_eq!(titles(cx), ["b:行情"]);
        assert!(changes.get() > seen);
        // 没设过的撤了也不算变。
        let seen = changes.get();
        cx.update(|cx| set_status_menu("nobody", None, cx));
        assert_eq!(changes.get(), seen);
    }

    #[test]
    fn menus_are_tagged_for_scripts() {
        let menu = StatusMenu {
            icon: Some(StatusImage::Asset("plugin-assets/example/1/bar.png".into())),
            title: "1@".into(),
            tooltip: String::new(),
            items: vec![
                StatusMenuItem::Action {
                    label: "会话".into(),
                    icon: Some(StatusIcon {
                        png: StatusImage::Png(vec![1, 2]),
                        color: 0x112233,
                    }),
                    command: "example/activate".into(),
                    args: serde_json::json!({ "machine": null, "session_id": "s" }),
                },
                StatusMenuItem::Separator,
            ],
        };
        let value = serde_json::to_value(&menu).unwrap();
        // 路径写成字符串，字节写成数组：脚本两种都能给。
        assert_eq!(value["icon"], "plugin-assets/example/1/bar.png");
        assert_eq!(value["items"][0]["type"], "action");
        assert_eq!(value["items"][0]["icon"]["png"], serde_json::json!([1, 2]));
        assert_eq!(value["items"][0]["icon"]["color"], "#112233");
        assert_eq!(
            value["items"][1],
            serde_json::json!({ "type": "separator" })
        );
        assert_eq!(serde_json::from_value::<StatusMenu>(value).unwrap(), menu);
        // 脚本可以只给菜单项，项里可以不带图标和参数。
        let bare: StatusMenu = serde_json::from_value(serde_json::json!({
            "items": [{ "type": "action", "label": "打开", "command": "smelt.app/show-main-window" }],
        }))
        .unwrap();
        assert_eq!(bare.title, "");
        assert_eq!(bare.icon, None);
        assert!(matches!(
            &bare.items[0],
            StatusMenuItem::Action {
                icon: None,
                args: serde_json::Value::Null,
                ..
            }
        ));
    }
}
