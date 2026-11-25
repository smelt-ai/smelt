//! 宿主手里解码好的图（如 Rust 插件从系统读到的应用图标）交给脚本时，给一个资源路径，不给
//! 字节。字节过插件桥要拆成数字数组，大一点的图就超过桥上数组的上限，每次重画还要来回拷一遍。
//! 脚本拿到的路径和 `asset()` 给的一样用（`image(path)`、`SmeltMenuItem.picture(path)`），应用的
//! 资源表按路径回到这里取字节（[`load_image_asset`]）。
//!
//! 表里只记弱引用：图归给出它的那一方（如菜单登记），撤下了路径就取不到，画出来是空的。

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, Weak};

/// 宿主给的图的路径开头，和 Smelt 自己的资源、插件目录里的文件分开。
const PREFIX: &str = "host-images/";

/// 图的 id → 图。GPUI 在后台线程取资源，所以放进程级的表。
static IMAGES: LazyLock<Mutex<HashMap<u64, Weak<gpui::Image>>>> = LazyLock::new(Default::default);

fn images() -> std::sync::MutexGuard<'static, HashMap<u64, Weak<gpui::Image>>> {
    IMAGES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 给脚本用的这张图的资源路径。同一张图每次给的一样；顺手清掉已经没人持有的。
pub fn image_asset(image: &Arc<gpui::Image>) -> String {
    let mut images = images();
    images.retain(|_, image| image.strong_count() > 0);
    images.insert(image.id, Arc::downgrade(image));
    format!("{PREFIX}{}", image.id)
}

/// 资源表取一个路径时先问这里：不是宿主给的图的路径是 `None`；是的话，图还在给字节，
/// 撤下了给 `Some(None)`。
pub fn load_image_asset(path: &str) -> Option<Option<Vec<u8>>> {
    let id = path.strip_prefix(PREFIX)?;
    let image = id
        .parse::<u64>()
        .ok()
        .and_then(|id| images().get(&id)?.upgrade());
    Some(image.map(|image| image.bytes.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_finds_the_image_while_someone_holds_it() {
        let image = Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, vec![1, 2]));
        let path = image_asset(&image);
        assert_eq!(image_asset(&image), path);
        assert_eq!(load_image_asset(&path), Some(Some(vec![1, 2])));
        drop(image);
        assert_eq!(load_image_asset(&path), Some(None));
        assert_eq!(load_image_asset("host-images/not-a-number"), Some(None));
        assert_eq!(load_image_asset("icons/plus.svg"), None);
    }
}
