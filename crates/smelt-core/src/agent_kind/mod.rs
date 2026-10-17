//! Agent 身份标识的类型在 `smelt_model::agent_kind`；这里补上运行时那一侧要读文件、
//! 起进程的部分（DSH 的 profile、桥和插件）。

pub use smelt_model::agent_kind::*;

mod dsh;
pub use dsh::*;
