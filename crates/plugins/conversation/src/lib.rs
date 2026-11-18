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
mod menu;
mod sessions;
mod view;

use smelt_plugin_api as api;

rust_i18n::i18n!("locales", fallback = "en");

/// 按数量挑单复数的那一句：`one` 是 1 的说法，`other` 是别的。两句都由调用方用字面量键
/// 取好，键检查才认得出来。
pub(crate) fn plural(
    count: usize,
    one: std::borrow::Cow<'static, str>,
    other: std::borrow::Cow<'static, str>,
) -> String {
    if count == 1 { one } else { other }.into_owned()
}

/// 来源 id。
pub const OWNER: &str = "smelt.conversation";

/// 对话的图标（侧栏「对话」里每段对话也是它）。
const ICON: &str = api::ui::IconName::MessageSquare.asset_path();

/// 内置插件的入口：宿主启动时按它登记（见 [`smelt_plugin_api::BuiltinPlugin`]）。
pub const PLUGIN: smelt_plugin_api::BuiltinPlugin = smelt_plugin_api::BuiltinPlugin {
    id: OWNER,
    register,
};

/// 宿主启动时调一次。
fn register(cx: &mut gpui::App) {
    if let Err(error) = api::register_topics(
        OWNER,
        [api::model::daemon_state::CORE_TOPIC_PI_CONFIG_CHANGED],
        cx,
    ) {
        eprintln!("[conversation] 话题登记失败：{error}");
    }
    agents::install(cx);
    if let Err(error) = api::register_views(OWNER, vec![instance::spec()], cx) {
        eprintln!("[conversation] 视图登记失败：{error}");
    }
    if let Err(error) = api::register_commands(
        OWNER,
        api::text!("title"),
        vec![
            contract::INSERT_TEXT.spec(api::text!("command.insert_text"), |text, window, cx| {
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
            contract::MESSAGE_MENU,
        ] {
            assert_eq!(
                id.split_once('/').map(|(owner, _)| owner),
                Some(OWNER),
                "{id}"
            );
        }
    }
}
