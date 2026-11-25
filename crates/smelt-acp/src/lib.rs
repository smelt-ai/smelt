//! Smelt 的纯非 UI ACP 对话能力层：连接 `smeltd`、归约增量帧、管理多订阅复用与图片缓存。
//!
//! 严禁依赖 GPUI、严禁认识任何具体插件 id、严禁引入界面文案。

pub mod client;
pub mod hub;
pub mod images;

pub use client::{
    ACP_HISTORY_PAGE_LIMIT, ACP_INITIAL_TAIL_LIMIT, AcpClientError, ConversationClientEvent,
    ConversationClientHandle, ConversationClientLaunch, load_acp_history, restart_acp_session,
    spawn_acp_client,
};
pub use hub::{AcpEventSink, AcpHub, SubscribeRequest};
pub use images::{ImageStore, StoredImageMeta};
