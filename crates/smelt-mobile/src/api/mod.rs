//! 暴露给 Flutter 的 API。`flutter_rust_bridge.yaml` 的 `rust_input` 只扫这里（含子模块），
//! 这里只放 Dart 用得到的视图模型和入口，实现都在 crate 内部模块。

use flutter_rust_bridge::frb;

pub mod agents;
pub mod automations;
pub mod catalog;
pub mod conversation;
pub mod draft;
pub mod error;
pub mod files;
pub mod fleet;
pub mod identity;
pub mod pass;
pub mod push;
pub mod terminal;

/// App 启动时由 FRB 调用一次。
#[frb(init)]
pub fn init_app() {
    #[cfg(target_os = "android")]
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Debug),
    );
    // Flutter 的界面语言跟随系统；Rust 这边说的话（运行时报错的原因）也按系统语言来。
    let locale = sys_locale::get_locale().map_or(smelt_model::locale::FALLBACK_LOCALE, |tag| {
        smelt_model::locale::supported_locale(&tag)
    });
    rust_i18n::set_locale(locale);
    log::info!("smelt-mobile initialized");
}
