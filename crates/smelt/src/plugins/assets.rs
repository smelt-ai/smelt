//! 应用的资源表：Smelt 自己的内置资源（`smelt_ui::assets`），加上宿主解码后交给菜单的图像
//! （`host-images/…`，见 [`smelt_plugin_api::image_asset`]）。

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

use gpui::{AssetSource, SharedString};

/// 报过「找不到」的路径；GPUI 每次绘制都会再要，只报一次。
static REPORTED: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

/// 交给 GPUI 的资源表。
pub(crate) struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        // 撤下了的图：给它的菜单项已经不在，不报。
        if let Some(bytes) = smelt_plugin_api::load_image_asset(path) {
            return Ok(bytes.map(Cow::Owned));
        }
        if let Some(bytes) = smelt_ui::assets::load(path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        report_unknown(path);
        Ok(None)
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        smelt_ui::assets::Assets.list(path)
    }
}

fn report_unknown(path: &str) {
    let first = REPORTED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(path.to_string());
    if first {
        eprintln!("[plugins] 资源「{path}」不存在。Smelt 自带的图标是 icons/<名字>.svg");
    }
}

#[cfg(test)]
mod tests {
    use gpui::AssetSource as _;

    use super::Assets;

    #[test]
    fn loads_smelt_ui_icons_and_missing_returns_none() {
        let icon = Assets.load("icons/folder.svg").unwrap();
        assert!(icon.is_some(), "内置图标应可加载");
        let missing = Assets.load("icons/__nonexistent_smelt_icon__.svg").unwrap();
        assert!(missing.is_none());
    }
}
