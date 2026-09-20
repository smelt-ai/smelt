//! 交接 v2：双进程事务（predecessor spawn successor，经 socketpair +
//! `SCM_RIGHTS` 传 manifest 与 fd，READY/COMMIT 两阶段提交，COMMIT 前任何
//! 失败都回滚、老进程原地继续服务）。
//!
//! - [`manifest`]：版本化、分级、校验的 manifest 编解码（纯函数，本模块先行）。
//! - transport/predecessor/successor 后续补齐；legacy 文件读端保留一代兼容。

pub mod child_monitor;
pub mod manifest;
pub mod predecessor;
pub mod successor;
pub mod transport;
