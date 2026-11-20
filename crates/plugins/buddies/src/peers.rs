//! 用到的别的插件的约定，照抄自它们的 `contract.rs`，权威定义以那边为准：填的插槽。

/// 会话列表（来源 `smelt.session-list`）的约定，权威定义在
/// `plugins/session-list/contract.js`。
pub(crate) mod session_list {
    use serde::Deserialize;
    use smelt_plugin_api::{RuntimeKey, machine_json};

    // 约定：smelt.session-list 的 SESSION_AVATAR。
    /// 会话列表每行行首的头像（插槽），每行一份实例，参数是 [`SessionAvatarArgs`]。画在 36pt
    /// 见方的格子里，零件可以伸出去一点；右下角的小圆标归会话画。只用第一个填进来的。
    pub(crate) const SESSION_AVATAR: &str = "smelt.session-list/session-avatar";

    /// [`SESSION_AVATAR`] 的参数。
    #[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
    pub(crate) struct SessionAvatarArgs {
        #[serde(with = "machine_json")]
        pub machine: RuntimeKey,
        /// 工作区目录里那段会话的 id。
        pub conversation: String,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smelt_plugin_api::RuntimeKey;

    #[test]
    fn session_avatar_args_parse_from_the_owners_shape() {
        let avatar: session_list::SessionAvatarArgs =
            serde_json::from_value(serde_json::json!({ "machine": null, "conversation": "c" }))
                .unwrap();
        assert_eq!(avatar.machine, RuntimeKey::Local);
        assert_eq!(avatar.conversation, "c");
    }
}
