//! 远程访问的持久化配置（主库 `collab` 作用域）。
//!
//! 守护每次启动（冷启动、被重新拉起、无缝升级 exec 后的新进程）内存里都没有在跑的
//! endpoint，按这份配置自己拉起来，不等任何客户端。

pub use smelt_model::remote_config::*;

use crate::sqlite_state;

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
    }
}

fn config_from_snapshot(snapshot: smelt_store::RemoteConfigSnapshot) -> RemoteConfig {
    RemoteConfig {
        enabled: snapshot.enabled,
        iroh_relay: snapshot.iroh_relay,
    }
}
