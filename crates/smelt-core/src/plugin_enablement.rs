//! 宿主级插件启用状态。
//!
//! 未出现在关闭名单里的插件默认启用。
//! 状态只写 SQLite KV，不落 JSON 文件。

use serde::{Deserialize, Serialize};
use smelt_plugin_api::PluginId;
use std::collections::BTreeSet;

const NAMESPACE: &str = "plugins";
const KEY: &str = "enablement";

/// 用户改过的插件开关。缺省全部启用。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginEnablement {
    #[serde(default)]
    disabled: BTreeSet<String>,
}

impl PluginEnablement {
    /// 从 SQLite 读取；库未启用、键缺失或内容损坏时回退为全部启用。
    pub fn load() -> Self {
        match crate::sqlite_state::load_sqlite_kv::<Self>(NAMESPACE, KEY) {
            Ok(Some(value)) => value,
            Ok(None) => Self::default(),
            Err(error) => {
                if error.contains("解析失败") {
                    eprintln!("[plugins] {error}");
                }
                Self::default()
            }
        }
    }

    /// 写回 SQLite KV。SQLite 未启用时返回错误，调用方按需忽略。
    pub fn save(&self) -> Result<(), String> {
        crate::sqlite_state::save_sqlite_kv(NAMESPACE, KEY, self)
    }

    pub fn is_enabled(&self, plugin_id: &str) -> bool {
        !self.disabled.contains(plugin_id)
    }

    pub fn set_enabled(&mut self, plugin_id: &str, enabled: bool) {
        let plugin_id = plugin_id.trim();
        if plugin_id.is_empty() {
            return;
        }
        if enabled {
            self.disabled.remove(plugin_id);
        } else {
            self.disabled.insert(plugin_id.to_string());
        }
    }

    pub fn disabled_plugin_ids(&self) -> BTreeSet<PluginId> {
        self.disabled
            .iter()
            .filter_map(|id| PluginId::new(id.clone()).ok())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_enables_all_plugins() {
        let enablement = PluginEnablement::default();
        assert!(enablement.is_enabled("com.example.plugin"));
        assert!(enablement.disabled_plugin_ids().is_empty());
    }

    #[test]
    fn disabling_a_plugin_is_opt_out_and_round_trips() {
        let mut enablement = PluginEnablement::default();
        enablement.set_enabled("com.example.plugin", false);
        enablement.set_enabled("com.example.other", true);
        enablement.set_enabled("  ", false);

        assert!(!enablement.is_enabled("com.example.plugin"));
        assert!(enablement.is_enabled("com.example.other"));

        let json = serde_json::to_string(&enablement).unwrap();
        let loaded: PluginEnablement = serde_json::from_str(&json).unwrap();
        assert!(!loaded.is_enabled("com.example.plugin"));
        assert_eq!(
            loaded.disabled_plugin_ids(),
            BTreeSet::from([PluginId::new("com.example.plugin").unwrap()])
        );
    }

    #[test]
    fn ignores_legacy_opt_in_list() {
        let loaded: PluginEnablement =
            serde_json::from_str(r#"{"disabled":[],"enabled":["com.example.opt-in"]}"#).unwrap();
        assert!(loaded.is_enabled("com.example.opt-in"));
        assert!(loaded.disabled_plugin_ids().is_empty());
    }

    #[test]
    fn load_defaults_when_sqlite_is_not_enabled() {
        assert_eq!(PluginEnablement::load(), PluginEnablement::default());
    }
}
