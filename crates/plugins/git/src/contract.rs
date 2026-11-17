//! 变更（来源 `smelt.git`）对外的约定：别的插件能调的命令。
//!
//! 这里是权威定义。插件之间不互相依赖：调用方各自照抄用到的 id 和字段，注明出处。改这里
//! 就是改线上形状，同一次改动里把照抄的各处一起改掉（`grep -rn 'smelt.git/' crates plugins`）。

use serde::{Deserialize, Serialize};
use smelt_plugin_api::{Command, RuntimeKey, machine_json};

/// 在浏览器里打开某个仓库当前分支的新建 PR 页。开不了时（还没推送、就在默认分支、认不出
/// 远端）命令失败，原因给人看。
pub(crate) const OPEN_PULL_REQUEST: Command<OpenPullRequest, ()> =
    Command::new("smelt.git/open-pull-request");

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct OpenPullRequest {
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 仓库根目录（工作树里就是那个仓库的 worktree 目录）。
    pub root: String,
}
