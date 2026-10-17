//! 升级后第一次启动：把以前存在运行时库里的客户端数据取回客户端库。
//!
//! 外观、更新设置、通知开关、侧栏和布局归客户端（设计文档 D1/D5）。旧版本把它们
//! 和运行时的数据存在同一个库里；GUI 不再打开运行时的库，只能经运行时取一次。
//! 迁移完成之前客户端库里的条目只是过渡期写下的，以运行时交回的旧值为准；取成功才
//! 记下「已迁移」，失败下次启动再试。

use smelt_client_store::{ClientStore, keys};
use smelt_model::control_api::MigrationTakeClientState;
use smelt_runtime_client::protocol::control::{ControlClientError, ControlErrorCode, EmptyParams};

pub(crate) fn migrate_client_state_once() {
    let Ok(store) = smelt_client_store::client_store() else {
        return;
    };
    let (scope, key) = keys::RUNTIME_MIGRATED;
    if matches!(store.get::<bool>(scope, key), Ok(Some(true))) {
        return;
    }
    // 只有这一次要等本机守护起来；之后的启动都只读客户端库。
    crate::terminal::ensure_daemon_running();
    let take = || {
        crate::runtime_call::call::<MigrationTakeClientState>(
            EmptyParams::default(),
            crate::runtime_call::RUNTIME_CALL_TIMEOUT,
        )
    };
    // 刚装好新版时，跑着的往往还是旧守护，它不认识这个方法。先无缝升级守护
    // （会话不断）再取一次；升级不了（有 ACP 回合在跑）就留到下次启动。
    let taken = match take() {
        Err(ControlClientError::Protocol(error))
            if error.code == ControlErrorCode::MethodNotFound =>
        {
            crate::terminal::upgrade_daemon();
            take()
        }
        other => other,
    };
    let legacy = match taken {
        Ok(legacy) => legacy,
        Err(error) => {
            eprintln!(
                "[client-state] 取回旧的客户端数据失败，下次启动再试：{}",
                crate::runtime_call::error_text(error)
            );
            return;
        }
    };
    adopt(store, keys::APPEARANCE, legacy.appearance);
    adopt(store, keys::UPDATE_SETTINGS, legacy.update_settings);
    adopt(store, keys::NOTIFY_PREFS, legacy.notify_prefs);
    adopt(store, keys::WORKSPACE_UI, legacy.workspace_ui);
    if let Err(error) = store.put(scope, key, &true) {
        eprintln!("[client-state] 记录迁移完成失败：{error}");
    }
}

fn adopt(store: &ClientStore, (scope, key): (&str, &str), value: Option<serde_json::Value>) {
    let Some(value) = value else {
        return;
    };
    if let Err(error) = store.put(scope, key, &value) {
        eprintln!("[client-state] 写入 {scope}/{key} 失败：{error}");
    }
}
