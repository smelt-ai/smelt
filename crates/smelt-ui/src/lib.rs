//! 共享 GPUI UI 基建：主 GUI 与 NSH 外壳共用。跟 `smelt-core` 的分工——那边不许引 GPUI，
//! 这边允许，但只放「不专属某个具体页面、被多处复用」的主题、图标、安全区、图片/Mermaid 渲染与设计令牌。

pub mod assets;
pub mod button;
pub mod frame_stats;
pub mod icon;
pub mod image;
pub mod keybind;
pub mod kit;
pub mod markdown_mermaid;
pub mod theme_catalog;
pub mod theme_config;
pub mod ui_theme;
pub mod window_bar;

pub use icon::{IconName, IconNamed};

rust_i18n::i18n!("locales", fallback = "en");

/// 装上本库的全局件：内置界面字体 Inter。应用启动时、开窗口之前调一次。
pub fn init(cx: &mut gpui::App) {
    // 字体随二进制编进来，载不进是构建出了错，不是运行时能恢复的情况。
    cx.text_system()
        .add_fonts(kit::UI_FONTS.map(std::borrow::Cow::Borrowed).to_vec())
        .expect("载入内置 Inter 字体失败");
}
