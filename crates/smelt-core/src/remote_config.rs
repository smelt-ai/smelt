//! 远程访问的持久化配置（主库 `collab` 作用域；残留 JSON 只导入一次）。
//!
//! 放在 smelt-core 而不是 GUI 的 settings 模块：守护也要读它。守护每次启动
//! （冷启动、`shutdown` 后被拉起、无缝升级 exec 后的新进程）内存里的网关与
//! 隧道状态都是空的，若没有这份落盘配置，远程就只能等 GUI 冷启动那一次去
//! `remote_start`/`iroh_start`——守护单独重启后手机就再也连不上，必须去设置页
//! 把远程「关掉再打开」才恢复。守护自己读配置自愈，才能覆盖所有重启路径。
//!
//! 只存开关和 relay 地址。设备配对 token 由守护单独存在 owner-only 文件里，
//! 不进这里。

use crate::sqlite_state;

/// 远程访问开关。字段语义与 GUI 设置页一一对应。
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RemoteConfig {
    /// 用户是否希望远程访问开着。守护启动时按它决定要不要自动拉起网关和隧道。
    #[serde(default)]
    pub enabled: bool,
    /// 用户自己的 iroh relay。空值表示未配置，不会回退到公共 relay。
    #[serde(default)]
    pub iroh_relay: String,
    /// 这条链接是否允许 approve/deny/reply。`#[serde(default)]`：比 `enabled`
    /// 更晚加，旧配置缺省按只读处理——不能让老用户的配置在升级后突然变成可写。
    #[serde(default)]
    pub write_enabled: bool,
}

/// 读取配置；文档缺失/损坏回退默认（关闭）。
pub fn load() -> RemoteConfig {
    let Ok(store) = sqlite_state::default_sqlite_store() else {
        return RemoteConfig::default();
    };
    match store.get_remote_config_snapshot() {
        Ok(Some(snapshot)) => config_from_snapshot(snapshot),
        Ok(None) | Err(_) => RemoteConfig::default(),
    }
}

/// 写回配置。主库本身已是 `0600`。
pub fn save(config: &RemoteConfig) {
    if let Ok(store) = sqlite_state::default_sqlite_store()
        && let Err(error) = store.put_remote_config_snapshot(&snapshot_from_config(config))
    {
        eprintln!("[storage] 保存远程配置失败: {error}");
    }
}

fn snapshot_from_config(config: &RemoteConfig) -> smelt_store::RemoteConfigSnapshot {
    smelt_store::RemoteConfigSnapshot {
        enabled: config.enabled,
        iroh_relay: config.iroh_relay.clone(),
        write_enabled: config.write_enabled,
    }
}

fn config_from_snapshot(snapshot: smelt_store::RemoteConfigSnapshot) -> RemoteConfig {
    RemoteConfig {
        enabled: snapshot.enabled,
        iroh_relay: snapshot.iroh_relay,
        write_enabled: snapshot.write_enabled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_config_without_new_fields_still_parses() {
        let c: RemoteConfig =
            serde_json::from_str(r#"{"enabled":true}"#).expect("旧配置必须能解析");
        assert!(c.enabled);
        // 缺省必须是只读：升级不能把老用户的只读链接变成可写。
        assert!(!c.write_enabled);
        assert!(c.iroh_relay.is_empty());
    }
}
