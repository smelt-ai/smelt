//! `process.run`：在运行时那台机器上跑一个程序，等它结束，交回退出码和输出。
//!
//! 和 `fs.*` 一样不按项目限范围：调用方（插件、手机、配对的机器）本来就可信，能写文件就
//! 能跑程序。程序按那台机器的登录 shell 环境找（`PATH` 等），`env` 叠加在上面。只给一问
//! 一答：不开 stdin、不推流式输出，要交互的用终端。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::control::{ControlCall, ControlMethod};

/// 跑一个程序直到退出。程序不存在报 `not_found`；超时整棵进程树被杀掉，`timed_out` 为真。
pub enum ProcessRunCall {}

impl ControlCall for ProcessRunCall {
    type Params = ProcessRunParams;
    type Result = ProcessRunResult;

    const METHOD: ControlMethod = ControlMethod::ProcessRun;
}

/// 不给 `timeout_ms` 时等这么久。
pub const PROCESS_RUN_DEFAULT_TIMEOUT_MS: u64 = 60_000;
/// `timeout_ms` 最多这么长，超过报 `invalid_params`。
pub const PROCESS_RUN_MAX_TIMEOUT_MS: u64 = 30 * 60_000;
/// stdout、stderr 各自最多交回这么多字节，多出来的读掉丢弃，`truncated` 为真。
pub const PROCESS_OUTPUT_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProcessRunParams {
    /// 程序：绝对路径，或在登录 shell 的 `PATH` 里找的名字。
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// 工作目录，绝对路径。不给是运行时自己的工作目录。
    #[serde(default)]
    pub cwd: Option<String>,
    /// 叠加在登录 shell 环境上的变量。
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// 最多等多少毫秒，不给是 [`PROCESS_RUN_DEFAULT_TIMEOUT_MS`]。
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProcessRunResult {
    /// 退出码。被信号结束（含超时被杀）时没有。
    pub code: Option<i32>,
    /// 按 UTF-8 解码，不合法的字节换成 U+FFFD。
    pub stdout: String,
    pub stderr: String,
    /// 到时间还没结束，已经被杀掉。
    pub timed_out: bool,
    /// stdout 或 stderr 超过 [`PROCESS_OUTPUT_LIMIT`]，只交回了前面一段。
    pub truncated: bool,
}
