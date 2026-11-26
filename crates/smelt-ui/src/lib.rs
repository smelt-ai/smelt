//! 共享 GPUI UI 基建：主 GUI 与宿主能力层共用。跟
//! `smelt-core` 的分工——那边不许引 GPUI，这边允许，但只放「不专属某个具体
//! 页面、被多处复用」的东西，别把整个应用的 UI 都塞进来。

pub mod assets;
mod base_theme;
pub mod button;
pub mod combobox;
pub mod command_palette;
pub mod dialog;
pub mod field;
pub mod frame_stats;
pub mod global_wait;
pub mod icon;
pub mod icon_action;
pub mod image;
pub mod keybind;
pub mod kit;
pub mod link;
pub mod list_row;
pub mod markdown_mermaid;
pub mod menu;
pub mod motion;
pub mod page_toolbar;
pub mod popover;
pub mod progress;
pub mod scrollbar;
pub mod segmented;
pub mod select;
pub mod settings;
pub mod slider;
pub mod switch;
pub mod theme_catalog;
pub mod theme_config;
pub mod tooltip;
pub mod truncated_line;
pub mod ui_theme;
pub mod window_bar;

pub use base_theme::set_code_font;
pub use button::Button;
pub use combobox::{Combobox, ComboboxGroup, ComboboxOption};
pub use command_palette::CommandPalette;
pub use field::{TextArea, TextInput};
pub use icon::{Icon, IconName, IconNamed};
pub use icon_action::IconAction;
pub use keybind::Keybind;
pub use link::Link;
pub use list_row::ListRow;
pub use page_toolbar::page_toolbar;
pub use progress::Progress;
pub use segmented::{Segment, Segmented};
pub use select::Select;
pub use settings::{SettingsPage, SettingsRow, SettingsSection};
pub use switch::Switch;

rust_i18n::i18n!("locales", fallback = "en");

/// 装上本库的全局件：界面字体 Inter、菜单、下拉、可搜索列表键位、窗口对话框层。
/// 应用启动时、`gpui_base::init` 之后、开窗口之前调一次。
pub fn init(cx: &mut gpui::App) {
    // 字体随二进制编进来，载不进是构建出了错，不是运行时能恢复的情况。
    cx.text_system()
        .add_fonts(kit::UI_FONTS.map(std::borrow::Cow::Borrowed).to_vec())
        .expect("载入内置 Inter 字体失败");
    menu::init(cx);
    combobox::init(cx);
    dialog::init(cx);
}

/// 删除/移除统一用这枚（`kit::GLYPHS.trash`）。
pub fn trash_icon() -> IconName {
    kit::GLYPHS.trash
}
