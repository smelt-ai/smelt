//! 分屏直接用 gpui-component 的 `h_resizable` / `v_resizable`。
//!
//! 组件库的分隔条已经画了静止发丝，悬停、按下、拖拽时再长出药丸。
//! 这里不再叠第二根发丝。

pub(crate) use gpui_component::resizable::{h_resizable, v_resizable};
