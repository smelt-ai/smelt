//! 远程桌面已经拆成内置插件 `smelt-desktop`，入口（机器菜单里的一项）也由它自己登记。宿主
//! 里只剩两处例外，等它们改成通用机制后整个文件删掉：
//! - 侧栏顶上的入口不给它（`conversations`）：要先挑机器；
//! - 首页出口不存它（`workspace_layout`）：是现场，重启后不自动连回去。

/// 远程桌面视图的 id。
pub(crate) const DESKTOP_VIEW: &str = smelt_desktop::VIEW;
