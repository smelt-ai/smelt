//! 小人：会话行首会动的 3D 小人头像。
//!
//! 内置插件（来源 id [`OWNER`]），只依赖 [`smelt_plugin_api`] 和第三方库。不对外给约定，反过来
//! 填会话列表定的插槽 `smelt.session-list/session-avatar`（约定照抄在 `peers`）。会话列表不认识
//! 这个插件，删掉它行首回到图标。
//!
//! 插槽只给是哪段会话；会话在干什么、露没露着、跟哪个智能体都由这里看宿主的目录和会话状态
//! （`cast`）。小人不让人挑：智能体对话按智能体 id 落，别的按会话 id 落。
//!
//! - `buddy`：小人长什么样（零件、动作、谁在演哪一出）。零件由 `scripts/` 下的脚本渲好、打包，
//!   编进来。
//! - `cast`：全局状态，事件驱动，头像只读它。
//! - `avatar`：填插槽的视图，每段会话一份。
//! - `frames`：出帧的节拍，按固定帧率出、看不见的不要帧。

mod avatar;
mod buddy;
mod cast;
mod frames;
mod peers;

use smelt_plugin_api as api;

rust_i18n::i18n!("locales", fallback = "en");

/// 来源 id。
pub const OWNER: &str = "smelt.buddies";

/// 小人的图标。
const ICON: &str = api::ui::IconName::Sparkles.asset_path();

/// 宿主启动时调一次。
pub fn register(cx: &mut gpui::App) {
    cast::install(cx);
    if let Err(error) = api::register_views(OWNER, vec![avatar::spec()], cx) {
        eprintln!("[buddies] 视图登记失败：{error}");
    }
}
