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

/// 浏览运行时那台机器上的目录，给「打开项目」「绑定目录」这类选择用。本机运行时也走
/// 它：客户端从不假设文件就在自己机器上。隐藏项不列出。
pub enum WorkspaceBrowseDirsCall {}

impl ControlCall for WorkspaceBrowseDirsCall {
    type Params = BrowseDirsParams;
    type Result = BrowseDirsResult;

    const METHOD: ControlMethod = ControlMethod::WorkspaceBrowseDirs;
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowseDirsParams {
    /// 要列的目录，绝对路径。为空时列用户主目录。
    pub path: Option<String>,
    /// 连文件一起列（导入单文件插件时要选文件）。
    #[serde(default)]
    pub include_files: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowseDirsResult {
    /// 实际列出的目录（规范化后的绝对路径）。
    pub path: String,
    /// 上一级；已经在根目录时为空。
    pub parent: Option<String>,
    /// 用户主目录，给「回到主目录」用。
    pub home: String,
    /// 目录在前，各自按名字排序。
    pub entries: Vec<BrowseEntry>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowseEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
}
