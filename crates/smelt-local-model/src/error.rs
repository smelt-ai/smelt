use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O 错误：{0}")]
    Io(#[from] std::io::Error),
    #[error("网络请求失败：{0}")]
    Http(#[from] reqwest::Error),
    #[error("下载返回 HTTP {0}")]
    HttpStatus(u16),
    #[error("下载已取消")]
    Cancelled,
    #[error("文件大小不符：期望 {expected} 字节，实际 {actual} 字节")]
    SizeMismatch { expected: u64, actual: u64 },
    #[error("sha256 不符：期望 {expected}，实际 {actual}")]
    ChecksumMismatch { expected: String, actual: String },
    #[error("模型未安装：{0}")]
    NotInstalled(PathBuf),
    #[error("输入为空")]
    EmptyInput,
    #[error("提示词过长：{tokens} token，上下文仅允许 {limit}")]
    PromptTooLong { tokens: usize, limit: usize },
    #[error("模型没有产出可用标题")]
    NoTitle,
    #[error("推理后端错误：{0}")]
    Backend(String),
}

impl Error {
    pub(crate) fn backend(e: impl std::fmt::Display) -> Self {
        Self::Backend(e.to_string())
    }
}
