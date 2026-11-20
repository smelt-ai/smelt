//! 小人：会动的 3D 小人，一个元素 [`BuddyFigure`]。
//!
//! 只管画：哪个小人（按种子落，同一个种子永远是同一个）、演哪一出、从什么时候起演，都由用它的
//! 一方给，这里不认识会话。动作按时间现算，用的一方只在换戏时重画，演的过程这里按节拍自己要帧。
//!
//! - `buddy`：小人长什么样（零件、动作、合成）。零件由 `scripts/` 下的脚本渲好、打包，编进来。
//! - `frames`：出帧的节拍，按固定帧率出、看不见的不要帧。
//! - `element`：元素本身，按元素留着正在画的那一帧和节拍。

mod buddy;
mod element;
mod frames;

pub use buddy::Act;
pub use element::BuddyFigure;

/// 小人画在多大的见方格子里（pt）。零件（举起来的道具、头顶的特效）可以伸出去一点，不裁。
pub const SIZE: f32 = buddy::CANVAS;
