//! 消息草稿：每台机器的每个会话一份，存在本机库里（见 `crate::store`）。
//!
//! 都是普通 `pub fn`（FRB 放到工作线程池，图片可能有几 MB，不占 UI 线程）。工作线程池
//! 不保证先发先到：同一会话的草稿操作 Dart 侧要串行（等上一次完成再发下一次），否则
//! 先改的可能覆盖后改的。

use crate::api::conversation::ImageInput;
use crate::api::error::SmeltError;

/// 一条还没发出去（或还没等到确认）的消息。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageDraft {
    pub content: String,
    /// 按添加顺序。
    pub images: Vec<ImageInput>,
    /// 有值表示这份草稿发过、但运行时没确认收到：重发时必须用同一个 id，运行时按它去重。
    /// 用户改了内容就不再是同一次提交，存的时候要清掉。
    pub request_id: Option<String>,
}

/// 读草稿，没有时为空。
pub fn load_draft(
    runtime_id: String,
    session_id: String,
) -> Result<Option<MessageDraft>, SmeltError> {
    crate::fleet::fleet()
        .store()?
        .load_draft(&runtime_id, &session_id)
}

/// 存草稿，整份替换。没有文字（只有空白）也没有图时等于 [`clear_draft`]。机器没登记时报错。
pub fn save_draft(
    runtime_id: String,
    session_id: String,
    draft: MessageDraft,
) -> Result<(), SmeltError> {
    crate::fleet::fleet().store()?.save_draft(
        &runtime_id,
        &session_id,
        &draft,
        crate::store::now_ms(),
    )
}

pub fn clear_draft(runtime_id: String, session_id: String) -> Result<(), SmeltError> {
    crate::fleet::fleet()
        .store()?
        .clear_draft(&runtime_id, &session_id)
}

/// 一次发送有了结论时调：存着的草稿还属于这次发送（`request_id` 相同）才处理——成功就删掉，
/// 失败就清掉 `request_id`、留着内容。发送途中用户又改了草稿就不动它，返回 false。
pub fn resolve_draft(
    runtime_id: String,
    session_id: String,
    request_id: String,
    succeeded: bool,
) -> Result<bool, SmeltError> {
    crate::fleet::fleet()
        .store()?
        .resolve_draft(&runtime_id, &session_id, &request_id, succeeded)
}
