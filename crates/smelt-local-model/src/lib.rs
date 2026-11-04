//! 本地小模型：Qwen3.5-0.8B（GGUF，Q8_0 量化）。模型清单见 [`TitleModel`]，可按需扩展。
//!
//! 提供：
//! - 模型规格（固定仓库 revision、文件名、大小、sha256）；
//! - 下载（流式写临时文件 → 校验 → 原子发布）、完整性校验、卸载（删文件）；
//! - 一次性同步推理：加载 → 生成短标题 → 卸载，不常驻内存。
//!
//! 除 [`download_model`]（异步，需 tokio 运行时，可被 [`CancelToken`] 立即取消）外，
//! 本库函数都是阻塞的（磁盘、推理），调用方负责放到后台线程，不得在 UI 线程或 render 中调用。本库不做隐式下载：推理只读调用方给出的已安装路径。

mod download;
mod error;
mod infer;
mod spec;
mod title;

pub use download::{
    CancelToken, DownloadProgress, download_model, install_from_reader, is_installed, model_path,
    remove_model, verify_file,
};
pub use error::{Error, Result};
pub use infer::{TitleOptions, generate_title};
pub use spec::{ModelSpec, QWEN3_5_0_8B_Q8_0, TitleModel, TitleModelSpec};
pub use title::{build_title_prompt_parts, extract_title, truncate_input};
