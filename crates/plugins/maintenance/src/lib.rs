//! 维护：这台电脑上的杂务，不经运行时。
//!
//! - 设置「系统 → 清理残留」：`~/.smelt` 下只留登记过的条目，别的列出来确认后删掉
//!   （[`cleanup`]、[`residue`]）。
//! - 终端里的 `smelt` 命令（macOS）：启动时把 `~/.smelt/bin/smelt` 拨到正在跑的这个 App，
//!   智能体靠它敲得到 `smelt`；设置「系统 → 命令行」里可以再装 `/usr/local/bin/smelt`，
//!   人在终端里也敲得到（[`page`]）。
//!
//! 文件 IO 全在后台线程上做。内置插件，只经 [`smelt_plugin_api`] 和宿主打交道。

rust_i18n::i18n!("locales", fallback = "en");

mod cleanup;
#[cfg(target_os = "macos")]
mod cli_command;
#[cfg(target_os = "macos")]
mod page;
mod residue;

use gpui::App;

/// 本插件的来源 id。
const OWNER: &str = "smelt.maintenance";

/// 宿主启动时调一次。
pub fn register(cx: &mut App) {
    #[cfg(target_os = "macos")]
    sync_cli_link_at_startup(cx);
    // 登记按来源整组替换：本插件的页要一次登记，分开登记后一次会顶掉前一次。
    if let Err(error) = smelt_plugin_api::register_views(OWNER, views(), cx) {
        eprintln!("[maintenance] 设置页登记失败：{error}");
    }
}

fn views() -> Vec<smelt_plugin_api::ViewSpec> {
    vec![
        cleanup::spec(),
        #[cfg(target_os = "macos")]
        page::spec(),
    ]
}

/// 启动时在后台把 `~/.smelt/bin/smelt` 拨到这个 App。
#[cfg(target_os = "macos")]
fn sync_cli_link_at_startup(cx: &App) {
    cx.background_executor()
        .spawn(async {
            if let Err(error) = cli_command::sync_managed_link() {
                eprintln!("[maintenance] 放 ~/.smelt/bin/smelt 失败：{error}");
            }
        })
        .detach();
}

#[cfg(test)]
mod tests {
    /// 登记按来源整组替换，本插件的页必须在同一组里，少一页它就从设置里消失了。
    #[test]
    fn every_page_is_registered_in_one_set() {
        let names: Vec<String> = super::views()
            .into_iter()
            .map(|spec| spec.name.to_string())
            .collect();
        assert!(names.contains(&"cleanup".to_string()));
        #[cfg(target_os = "macos")]
        assert!(names.contains(&"cli".to_string()));
    }
}
