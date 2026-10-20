//! 指挥台：跨机器的单据与灶台。
//!
//! 内核把所有机器的会话列表投影成指挥台要的两样东西，界面只管画（实现见 `crate::pass`）：
//! - **单据**：等你处理的事（审批、输入、失败，或状态是 `NeedsYou`），先来的在前，带上
//!   取回的审批 / 询问详情，原地就能批、答。
//! - **灶台**：每台机器一组，在做、刚完成（成功且没读过）、最近跑过（成功且读过，最多 5 条）。

use crate::api::catalog::SessionSummary;
use crate::api::conversation::{ConversationEntry, Elicitation, PermissionRequest};
use crate::frb_generated::StreamSink;

/// 一张单据：一个在等你的会话和它在等什么。
// 只要 PartialEq：带着的上下文条目（ConversationEntry）不是 Eq。
#[derive(Clone, Debug, PartialEq)]
pub struct PassTicket {
    pub runtime_id: String,
    pub session: SessionSummary,
    /// 第一个待批的审批。详情没取回、或在等的不是审批时为空。
    pub permission: Option<PermissionRequest>,
    /// 在等的询问。
    pub elicitation: Option<Elicitation>,
    /// 详情取回来了（取回来是空的也算，比如失败本来就没有审批卡）。会话摘要里和「在等
    /// 什么」有关的字段一变就重取，取回前为 false，`permission` / `elicitation` 为空。
    pub details_loaded: bool,
    /// 会话最近几条，印在单据上当上下文（旧的在前）。详情没取回时为空。
    pub recent: Vec<ConversationEntry>,
    /// 从什么时候开始等（unix 毫秒）：会话最后一次活动的时间；会话没有活动时间时取内核
    /// 第一次看到这张单据的时间。单据按它从早到晚排。
    pub waiting_since: i64,
}

/// 一台机器的灶台。各组都按最后活动时间从新到旧排，同时间的保持会话列表的次序。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PassStation {
    pub runtime_id: String,
    /// 在做：状态是 `Running`。
    pub running: Vec<SessionSummary>,
    /// 刚完成：成功结束、提醒还没读。读过就挪到 `recent`。
    pub done: Vec<SessionSummary>,
    /// 最近跑过：成功结束、提醒已读，最多 5 条。
    pub recent: Vec<SessionSummary>,
    /// 这台机器的会话列表不是实时的（没连上，`saved_at_ms` 有值时来自离线缓存）。
    /// 非实时的列表不出单据：上面的事可能早处理完了。
    pub cached: bool,
    /// 缓存保存时间（unix 毫秒）。
    pub saved_at_ms: Option<i64>,
}

/// 整份指挥台。`stations` 按机器列表顺序，每台登记的机器一组。
#[derive(Clone, Debug, PartialEq)]
pub struct Pass {
    pub tickets: Vec<PassTicket>,
    pub stations: Vec<PassStation>,
}

/// 指挥台流：订阅时先推一次当前整份，之后有变化就整份推（和会话列表同一节奏合并）。
pub fn pass_events(sink: StreamSink<Pass>) {
    crate::fleet::fleet().subscribe_pass(Box::new(move |pass| {
        // Dart 侧取消订阅后端口关闭，发送失败，监听随之移除。
        sink.add(pass.clone()).is_ok()
    }));
}
