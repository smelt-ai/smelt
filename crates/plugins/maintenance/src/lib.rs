//! 维护：这台电脑上的杂务，不经运行时。
//!
//! - 启动时删掉 `~/.smelt` 下已经没人读写的废弃文件（[`residue`]）。
//! - 终端里的 `smelt` 命令（macOS）：启动时把 `~/.smelt/bin/smelt` 拨到正在跑的这个 App，
//!   智能体靠它敲得到 `smelt`；设置「系统 → 命令行」里可以再装 `/usr/local/bin/smelt`，
//!   人在终端里也敲得到（[`page`]）。
//!
//! 文件 IO 全在后台线程上做。内置插件，只经 [`smelt_plugin_api`] 和宿主打交道。

rust_i18n::i18n!("locales", fallback = "en");

#[cfg(target_os = "macos")]
mod cli_command;
#[cfg(target_os = "macos")]
mod page;
mod residue;

use gpui::App;

/// 本插件的来源 id。
#[cfg(target_os = "macos")]
const OWNER: &str = "smelt.maintenance";

/// 内置插件的入口：宿主启动时按它登记（见 [`smelt_plugin_api::BuiltinPlugin`]）。
pub const PLUGIN: smelt_plugin_api::BuiltinPlugin = smelt_plugin_api::BuiltinPlugin {
    id: OWNER,
    register,
};

/// 宿主启动时调一次。
fn register(cx: &mut App) {
    tidy_at_startup(cx);
    #[cfg(target_os = "macos")]
    page::register(cx);
}

/// 启动时在后台做一次：删掉没人再读写的废弃文件，把 `~/.smelt/bin/smelt` 拨到这个 App。
fn tidy_at_startup(cx: &App) {
    cx.background_executor()
        .spawn(async {
            let removed = residue::remove_obsolete_files_at_startup();
            if removed > 0 {
                eprintln!("[maintenance] 启动时清理了 {removed} 个历史残留文件");
            }
            #[cfg(target_os = "macos")]
            if let Err(error) = cli_command::sync_managed_link() {
                eprintln!("[maintenance] 放 ~/.smelt/bin/smelt 失败：{error}");
            }
        })
        .detach();
}
