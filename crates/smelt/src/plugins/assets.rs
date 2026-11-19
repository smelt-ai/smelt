//! 应用的资源表：Smelt 自己的资源，加上已挂载插件目录里的文件。
//!
//! GPUI 一个应用只有一张资源表，所有插件共用一个脚本运行时，画 `image(path)` 时已经不知道
//! 是哪个插件要的。所以插件的文件不按裸路径找，由脚本经 `asset(path)` 拿一个带身份的路径：
//! `plugin-assets/<插件 id>/<挂载代次>/<相对路径>`。
//!
//! - 插件 id 让两个插件各自的 `assets/logo.svg` 互不相干。
//! - 代次每挂一次换一个。GPUI 画过的 SVG 按路径留在图集里、图片（连同加载失败）按路径留在
//!   缓存里，都没法逐条清；插件重挂后路径换了，改过、删过、补上的文件按新路径重新取。旧路径
//!   不再认，旧图留在缓存里直到退出，只有开发时反复重挂才会攒。
//!
//! 别的路径是 Smelt 自己的（`icons/…`、`smelt-icons/…`，见 [`smelt_ui::assets`]）。
//! 读插件目录用 gpui-shell 的 [`AppAssets`]：限定在目录里、大小有上限、缺文件报一次。

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, RwLock};

use gpui::{AssetSource, SharedString};
use gpui_shell::AppAssets;

/// 插件资源路径的开头，和 Smelt 自己的资源分开。
const PREFIX: &str = "plugin-assets/";

/// 已挂载插件的目录：插件 id → 这次挂载。GPUI 在后台线程取资源，所以不放线程局部。
static MOUNTED: LazyLock<RwLock<HashMap<String, Mounted>>> = LazyLock::new(Default::default);
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
/// 报过「找不到」的路径；GPUI 每次绘制都会再要，只报一次。
static REPORTED: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

struct Mounted {
    generation: u64,
    files: AppAssets,
}

/// 插件 `id` 挂载（或重挂）在 `root`：之后它的资源按返回的前缀找。上一次挂载给的前缀不再认。
pub(super) fn mount(id: &str, root: PathBuf) -> String {
    let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
    let files = AppAssets::new(root);
    MOUNTED
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(id.to_string(), Mounted { generation, files });
    format!("{PREFIX}{id}/{generation}")
}

/// 插件卸下了：它的资源路径都不再认。
pub(super) fn unmount(id: &str) {
    MOUNTED
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(id);
}

/// 插件资源路径拆成：插件 id、代次、插件目录里的相对路径。不是插件资源返回 `None`。
fn split(path: &str) -> Option<(&str, u64, &str)> {
    let mut parts = path.strip_prefix(PREFIX)?.splitn(3, '/');
    let id = parts.next()?;
    let generation = parts.next()?.parse().ok()?;
    let file = parts.next()?;
    Some((id, generation, file))
}

/// 这次挂载的插件目录；插件没挂、或是旧挂载的路径返回 `None`。
fn files(id: &str, generation: u64) -> Option<AppAssets> {
    let mounted = MOUNTED
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let current = mounted.get(id)?;
    (current.generation == generation).then(|| current.files.clone())
}

/// 交给 GPUI 的资源表。应用和 `smelt plugin check` 都装它。
pub(crate) struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        if let Some((id, generation, file)) = split(path) {
            // 旧挂载的路径：那份视图已经撤了，或马上撤，不报。
            return match files(id, generation) {
                Some(files) => files.load(file),
                None => Ok(None),
            };
        }
        if let Some(bytes) = smelt_ui::assets::load(path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        report_unknown(path);
        Ok(None)
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        if let Some((id, generation, dir)) = split(path) {
            return match files(id, generation) {
                Some(files) => files.list(dir),
                None => Ok(Vec::new()),
            };
        }
        smelt_ui::assets::Assets.list(path)
    }
}

/// 谁都不认识的路径：多半是插件把自己目录里的文件直接写成了路径。画出来是空的，最难查，
/// 报一次说清楚该怎么写。
fn report_unknown(path: &str) {
    let first = REPORTED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(path.to_string());
    if first {
        eprintln!(
            "[plugins] 资源「{path}」不存在。插件目录里的文件要经 asset(\"{path}\") 取路径；\
             Smelt 自带的图标是 icons/<名字>.svg"
        );
    }
}

#[cfg(test)]
mod tests;
