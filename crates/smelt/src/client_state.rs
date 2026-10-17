//! 升级后第一次启动：把以前存在运行时库里的客户端数据取回客户端库。
//!
//! 外观、更新设置、通知开关、侧栏和布局归客户端（设计文档 D1/D5）。旧版本把它们
//! 和运行时的数据存在同一个库里；GUI 不再打开运行时的库，只能经运行时取一次。
//! 客户端库里已经有的条目不覆盖。取成功才记下「已迁移」，失败下次启动再试。

use smelt_client_store::{ClientStore, keys};
use smelt_model::control_api::MigrationTakeClientState;
use smelt_runtime_client::protocol::control::EmptyParams;

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
    let legacy = match crate::runtime_call::call::<MigrationTakeClientState>(
        EmptyParams::default(),
        crate::runtime_call::RUNTIME_CALL_TIMEOUT,
    ) {
        Ok(legacy) => legacy,
        Err(error) => {
            eprintln!(
                "[client-state] 取回旧的客户端数据失败，下次启动再试：{}",
                crate::runtime_call::error_text(error)
            );
            return;
        }
    };
    put_if_absent(store, keys::APPEARANCE, legacy.appearance);
    put_if_absent(store, keys::UPDATE_SETTINGS, legacy.update_settings);
    put_if_absent(store, keys::NOTIFY_PREFS, legacy.notify_prefs);
    put_if_absent(store, keys::WORKSPACE_UI, legacy.workspace_ui);
    if let Err(error) = store.put(scope, key, &true) {
        eprintln!("[client-state] 记录迁移完成失败：{error}");
    }
}

fn put_if_absent(
    store: &ClientStore,
    (scope, key): (&str, &str),
    value: Option<serde_json::Value>,
) {
    let Some(value) = value else {
        return;
    };
    if matches!(store.get::<serde_json::Value>(scope, key), Ok(None))
        && let Err(error) = store.put(scope, key, &value)
    {
        eprintln!("[client-state] 写入 {scope}/{key} 失败：{error}");
    }
}
