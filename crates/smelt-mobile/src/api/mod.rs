//! 暴露给 Flutter 的 API。`flutter_rust_bridge.yaml` 的 `rust_input` 只扫这里（含子模块），
//! 这里只放 Dart 用得到的视图模型和入口，实现都在 crate 内部模块。

use flutter_rust_bridge::frb;

pub mod catalog;
pub mod connection;
pub mod conversation;
pub mod error;
pub mod identity;
pub mod terminal;

/// App 启动时由 FRB 调用一次。
#[frb(init)]
pub fn init_app() {
    #[cfg(target_os = "android")]
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Debug),
    );
    log::info!("smelt-mobile initialized");
}
