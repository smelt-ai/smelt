//! 本机维护（沿用 `smelt.maintenance`）：设置「系统 → 清理残留」与（macOS）「系统 → 命令行」。
//! 启动时在 macOS 后台同步 `~/.smelt/bin/smelt` 指向当前 App。

mod cleanup;
#[cfg(target_os = "macos")]
mod cli_command;
#[cfg(target_os = "macos")]
mod page;
mod residue;

use gpui::App;

const OWNER: &str = "smelt.maintenance";

pub(crate) fn register(cx: &mut App) {
    #[cfg(target_os = "macos")]
    sync_cli_link_at_startup(cx);
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

#[cfg(target_os = "macos")]
fn sync_cli_link_at_startup(_cx: &App) {
    smelt_sys::blocking::spawn_detached(|| {
        if let Err(error) = cli_command::sync_managed_link() {
            eprintln!("[maintenance] 放 ~/.smelt/bin/smelt 失败：{error}");
        }
    });
}

#[cfg(test)]
mod tests {
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
