//! 工作区目录（项目列表、会话组和它们的启动参数）的业务方法。
//!
//! 目录归运行时：项目路径、会话的启动命令只在那台机器上有意义。客户端自己的界面
//! 状态（侧栏、布局、当前页）不在这里。保存是整份提交加版本号：基于旧版本的提交
//! 被拒，免得两个客户端互相覆盖。

use serde::{Deserialize, Serialize};

use super::control::{ControlCall, ControlMethod, EmptyParams};

/// 读取工作区目录和它当前的版本。从没保存过时 `document` 为 `None`。
pub enum WorkspaceLoadCall {}

impl ControlCall for WorkspaceLoadCall {
    type Params = EmptyParams;
    type Result = WorkspaceLoadResult;

    const METHOD: ControlMethod = ControlMethod::WorkspaceLoad;
}

/// 整份保存。`base_revision` 不是当前版本时回 `conflict`，不写入。
pub enum WorkspaceSaveCall {}

impl ControlCall for WorkspaceSaveCall {
    type Params = WorkspaceSaveParams;
    type Result = WorkspaceSaveResult;

    const METHOD: ControlMethod = ControlMethod::WorkspaceSave;
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceLoadResult {
    pub revision: u64,
    /// 工作区文档（`projects`、`sessions` 等）。格式由存储层定义并校验。
    pub document: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceSaveParams {
    /// 客户端上次读到或保存后的版本。
    pub base_revision: u64,
    pub document: serde_json::Value,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSaveResult {
    pub revision: u64,
}
