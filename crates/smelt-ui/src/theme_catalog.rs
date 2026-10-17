//! 把内置主题文件装进 gpui-kit 的 [`ThemeRegistry`]，再按名字 `apply_config`。
//!
//! 库启动时只带 Default Light / Default Dark。Ayu、Catppuccin 等在这里的
//! `themes/` 里，随二进制嵌入，不依赖进程的当前目录。Default 不在这些文件里，
//! `gpui_component::init` 已经装好。

use gpui::SharedString;
use gpui_component::{Theme, ThemeRegistry};

use crate::theme_files::THEME_FILES;
use crate::ui_theme;

/// 设置页下拉的 (值, 标签)。值是主题文件里的 `name`，例如 `Ayu Light`。
pub fn options() -> Vec<(SharedString, SharedString)> {
    smelt_model::theme_index::entries()
        .iter()
        .map(|entry| {
            (
                SharedString::from(entry.name.clone()),
                SharedString::from(entry.name.clone()),
            )
        })
        .collect()
}

/// 按名字装上主题，产品色跟着换成这份主题。名字不在目录里时返回 false。
pub fn apply(name: &str, cx: &mut gpui::App) -> bool {
    if !cx.has_global::<ThemeRegistry>() || !cx.has_global::<Theme>() {
        return false;
    }
    ensure_loaded(cx);
    let key = SharedString::from(name);
    let Some(config) = ThemeRegistry::global(cx).themes().get(&key).cloned() else {
        return false;
    };
    Theme::update(cx, |theme| {
        // 换主题是整份换上，不是在上一份上面改漏掉的字段。先回到库的默认
        // 圆角和阴影，主题文件写了的再盖上去。字号字体属于 Appearance。
        let font_family = theme.font_family.clone();
        let font_size = theme.font_size;
        let mono_family = theme.mono_font_family.clone();
        let mono_size = theme.mono_font_size;
        let scrollbar_mode = theme.scrollbar_mode;
        theme.radius = gpui::px(6.);
        theme.radius_lg = gpui::px(8.);
        theme.shadow = true;
        theme.apply_config(&config);
        theme.font_family = font_family;
        theme.font_size = font_size;
        theme.mono_font_family = mono_family;
        theme.mono_font_size = mono_size;
        theme.scrollbar_mode = scrollbar_mode;
        // 通知固定在右下角，避开左侧会话列表。主题文件不带这个位置。
        theme.notification.placement = gpui::Anchor::BottomRight;
    });
    ui_theme::adopt_active_theme(cx);
    true
}

fn ensure_loaded(cx: &mut gpui::App) {
    // Default Dark 启动时就在注册表里，不能拿它当「目录已装完」的标记。
    let loaded = SharedString::from("Ayu Dark");
    if ThemeRegistry::global(cx).themes().contains_key(&loaded) {
        return;
    }
    // 必须和 `gpui_component::init` 同一次 App::update 里装完。注册表观察者
    // 要等这次 update 末尾才激活。拖到后面再装，它会 `Theme::change`，
    // 用主题文件盖掉刚刚写上的字号。
    for file in THEME_FILES {
        if let Err(err) = ThemeRegistry::global_mut(cx).load_themes_from_str(file) {
            eprintln!("[theme] 加载主题失败: {err}");
        }
    }
}
