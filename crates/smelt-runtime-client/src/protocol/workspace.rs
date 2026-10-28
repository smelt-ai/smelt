//! 工作区相关的业务方法：浏览运行时那台机器上的目录。
//!
//! 工作区目录（项目列表、对话和它们的启动参数）归运行时，只经
//! `workspace.conversation.*`、`workspace.project.*` 单条改动，改完由运行时经
//! `workspace.changed` 推送整份目录。这几条调用在 `smelt_model::control_api`，参数和推送的
//! 类型在 `smelt_model::workspace_directory`。

use serde::{Deserialize, Serialize};

use super::control::{ControlCall, ControlMethod};

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
