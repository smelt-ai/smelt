//! 主题：随程序带的一批界面主题（Ayu、Catppuccin……）。
//!
//! 内置插件，只经 [`smelt_plugin_api`] 登记主题，和第三方主题走同一个目录、同一套接口。
//! 主题文件随插件编进二进制，不读磁盘。

use gpui::App;
use smelt_plugin_api as api;

/// 登记主题用的来源名。卸下时用它。
pub const OWNER: &str = "smelt.themes";

/// 随程序带的主题集文件（gpui-kit 主题集 JSON）。
pub static THEME_FILES: &[&str] = &[
    include_str!("../themes/adventure.json"),
    include_str!("../themes/alduin.json"),
    include_str!("../themes/asciinema.json"),
    include_str!("../themes/aurora.json"),
    include_str!("../themes/ayu.json"),
    include_str!("../themes/catppuccin.json"),
    include_str!("../themes/everforest.json"),
    include_str!("../themes/fahrenheit.json"),
    include_str!("../themes/flexoki.json"),
    include_str!("../themes/gruvbox.json"),
    include_str!("../themes/harper.json"),
    include_str!("../themes/hybrid.json"),
    include_str!("../themes/jellybeans.json"),
    include_str!("../themes/kibble.json"),
    include_str!("../themes/macos-classic.json"),
    include_str!("../themes/mellifluous.json"),
    include_str!("../themes/molokai.json"),
    include_str!("../themes/solarized.json"),
    include_str!("../themes/spaceduck.json"),
    include_str!("../themes/tokyonight.json"),
    include_str!("../themes/twilight.json"),
];

/// 宿主启动时调一次：在 `gpui_component::init` 之后、按设置装上主题之前。
/// 再调一次是整组替换，结果相同。
pub fn register(cx: &mut App) {
    api::register_theme_sets(OWNER, THEME_FILES, cx).expect("内置主题文件不合法或与已有主题重名");
}

/// 卸下这批主题。当前在用的主题不变，由宿主决定要不要换回默认。
pub fn unregister(cx: &mut App) {
    api::unregister_themes(OWNER, cx);
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    #[gpui::test]
    fn bundled_themes_cover_the_index(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            register(cx);
            // 再登记一次是替换，不算重名。
            register(cx);
            for source in THEME_FILES {
                let set: gpui_component::ThemeSet = api::serde_json::from_str(source).unwrap();
                for theme in set.themes {
                    assert!(
                        api::theme_contains(&theme.name, cx),
                        "{} 没登记上",
                        theme.name
                    );
                }
            }
            assert!(api::apply_theme("Ayu Dark", cx));
            unregister(cx);
            assert!(!api::theme_contains("Ayu Dark", cx));
            assert!(api::apply_theme("Default Dark", cx));
        });
    }
}
