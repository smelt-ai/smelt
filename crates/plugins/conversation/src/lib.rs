//! 对话：每段智能体对话是一个视图实例（[`contract`] 里的 `smelt.conversation/conversation`），
//! 接着守护里的一段 ACP 会话。agent 子进程、连接层和归约都在 smeltd 里，这里只消费推来的
//! 快照画消息流、权限审批卡片和输入框，把用户的操作打包发回去（见 `view` 的文件头）。
//!
//! 内置插件（来源 id [`OWNER`]），只依赖 [`smelt_plugin_api`] 和第三方库。摆对话、侧栏、
//! 开新对话和分叉、结束会话归摆它的一方（会话插件），经实例视图和 [`contract`] 里的命令、
//! 事件用这里；对话要别处做的事（开新对话、去智能体页、记住会话选项）按对方的约定转出去
//! （`instance`、`sessions`、`agents`）。

mod acp_client;
mod agents;
mod contract;
mod instance;
mod sessions;
mod view;

use smelt_plugin_api as api;

/// 来源 id。
pub const OWNER: &str = "smelt.conversation";

/// 对话的图标（Lucide `message-square`，侧栏「对话」里每段对话也是它）。
const ICON: &str = "icons/message-square.svg";

/// 宿主启动时调一次。
pub fn register(cx: &mut gpui::App) {
    agents::install(cx);
    if let Err(error) = api::register_views(OWNER, vec![instance::spec()], cx) {
        eprintln!("[conversation] 视图登记失败：{error}");
    }
    if let Err(error) = api::register_commands(
        OWNER,
        "对话",
        vec![
            contract::INSERT_TEXT.spec("填进对话输入框", |text, window, cx| {
                instance::insert_text(text, window, cx)
            }),
        ],
        cx,
    ) {
        eprintln!("[conversation] 命令登记失败：{error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_ids_are_under_this_owner() {
        for id in [
            contract::VIEW,
            contract::INSERT_TEXT.id(),
            contract::AT_LATEST_CHANGED.id(),
        ] {
            assert_eq!(
                id.split_once('/').map(|(owner, _)| owner),
                Some(OWNER),
                "{id}"
            );
        }
    }
}
