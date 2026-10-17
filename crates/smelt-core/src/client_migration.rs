//! `migration.take_client_state`：把以前存在运行时库里的客户端数据交还给桌面。
//!
//! 外观、更新设置、通知开关、侧栏和布局归客户端（设计文档 D1/D5），现在存在桌面
//! 自己的 `client.sqlite3`。升级后第一次启动时客户端库是空的，桌面经这个方法取一次
//! 旧值。旧行留在运行时库里不再被读写，不影响任何功能。

pub use smelt_model::client_migration::*;

use serde_json::{Value, json};

fn kv_json(store: &smelt_store::Store, namespace: &str, key: &str) -> Option<Value> {
    let raw = store.get_blob(namespace, key).ok()??;
    serde_json::from_slice(&raw).ok()
}

pub fn take_on(store: &smelt_store::Store) -> LegacyClientState {
    let appearance = store.get_appearance_snapshot().ok().flatten().map(|s| {
        json!({
            "bg_color": s.bg_color,
            "bg_image": s.bg_image,
            "bg_image_opacity": s.bg_image_opacity,
            "opacity": s.opacity,
            "blur": s.blur,
            "glass_style": s.glass_style,
            "theme_mode": s.theme_mode,
            "theme_name": s.theme_name,
            "ui_font_px": s.ui_font_px,
            "ui_font_family": s.ui_font_family,
            "font_px": s.font_px,
            "font_family": s.font_family,
        })
    });
    let update_settings = store
        .get_update_settings_snapshot()
        .ok()
        .flatten()
        .map(|s| json!({ "channel": s.channel, "auto_install": s.auto_install }));
    let notify_prefs = kv_json(store, "client", "notify_prefs").or_else(|| {
        store.get_agent_ui_snapshot().ok().flatten().map(|s| {
            json!({
                "approval": s.notify_approval,
                "input": s.notify_input,
                "success": s.notify_success,
                "failure": s.notify_failure,
                "terminal_bell": s.notify_terminal_bell,
            })
        })
    });
    let workspace_ui = kv_json(store, "client", "workspace_ui").or_else(|| {
        let snapshot = store.get_workspace_snapshot().ok()??;
        let raw = smelt_store::Store::workspace_snapshot_to_json(&snapshot).ok()?;
        serde_json::from_slice(&raw).ok()
    });
    LegacyClientState {
        appearance,
        update_settings,
        notify_prefs,
        workspace_ui,
    }
}

pub fn take() -> Result<LegacyClientState, String> {
    Ok(take_on(&crate::sqlite_state::default_sqlite_store()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_runtime_has_nothing_to_hand_back() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            smelt_store::Store::open_or_create(dir.path().join(smelt_store::DATABASE_FILE_NAME))
                .unwrap();
        assert_eq!(take_on(&store), LegacyClientState::default());
    }

    #[test]
    fn stored_appearance_comes_back_with_the_client_field_names() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            smelt_store::Store::open_or_create(dir.path().join(smelt_store::DATABASE_FILE_NAME))
                .unwrap();
        let snapshot = smelt_store::AppearanceSnapshot {
            bg_color: 0x1a1b26,
            bg_image: Some("/tmp/bg.png".into()),
            bg_image_opacity: 0.25,
            opacity: 0.95,
            blur: true,
            glass_style: "clear".into(),
            theme_mode: "light".into(),
            theme_name: "Ayu Light".into(),
            ui_font_px: 18,
            ui_font_family: String::new(),
            font_px: 13,
            font_family: String::new(),
        };
        store
            .modify_appearance_snapshot(
                || snapshot.clone(),
                |_| Ok::<(), smelt_store::StoreError>(()),
            )
            .unwrap();

        let appearance = take_on(&store).appearance.unwrap();
        assert_eq!(appearance["ui_font_px"], 18);
        assert_eq!(appearance["theme_mode"], "light");
        assert_eq!(appearance["glass_style"], "clear");
        // 客户端按这组字段名读外观（桌面 `Appearance` 的 serde 名），少一个就丢一项。
        let mut keys: Vec<_> = appearance.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "bg_color",
                "bg_image",
                "bg_image_opacity",
                "blur",
                "font_family",
                "font_px",
                "glass_style",
                "opacity",
                "theme_mode",
                "theme_name",
                "ui_font_family",
                "ui_font_px",
            ]
        );
    }

    /// 更早的版本把侧栏、当前页和工作区目录存在同一份快照里；客户端取回整份，
    /// 只认它要的字段。
    #[test]
    fn old_workspace_snapshot_carries_the_client_ui_state() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            smelt_store::Store::open_or_create(dir.path().join(smelt_store::DATABASE_FILE_NAME))
                .unwrap();
        let snapshot = smelt_store::Store::workspace_snapshot_from_json(
            br#"{"active_session_id":"sid-1","pinned_projects":["/repo"],"sidebar_hide_empty_projects":true,"sessions":[]}"#,
        )
        .unwrap();
        store.put_workspace_snapshot(&snapshot).unwrap();

        let ui = take_on(&store).workspace_ui.unwrap();
        assert_eq!(ui["active_session_id"], "sid-1");
        assert_eq!(ui["pinned_projects"], json!(["/repo"]));
        assert_eq!(ui["sidebar_hide_empty_projects"], true);
    }
}
