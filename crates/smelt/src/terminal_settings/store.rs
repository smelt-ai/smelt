//! 终端偏好数据库（`~/.smelt/plugin-data/smelt.terminal/store.sqlite3`）：终端字号。

use std::sync::Arc;

use smelt_plugin_api::rusqlite::{OptionalExtension as _, params};
use smelt_plugin_api::{PluginStore, plugin_store};
use smelt_terminal::{DEFAULT_FONT_PX, MAX_FONT_PX, MIN_FONT_PX};

use super::OWNER;

const MIGRATIONS: &[&str] = &["
    CREATE TABLE settings (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        font_px INTEGER NOT NULL
    );
"];

pub(crate) struct TerminalStore {
    store: Option<Arc<PluginStore>>,
}

impl TerminalStore {
    pub(crate) fn open() -> Self {
        let store = plugin_store(OWNER).and_then(|store| {
            store.migrate(MIGRATIONS)?;
            Ok(store)
        });
        match store {
            Ok(store) => Self { store: Some(store) },
            Err(error) => {
                eprintln!("[terminal] 打不开终端配置库，字号不保存：{error}");
                Self { store: None }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn open_at(path: &std::path::Path) -> Self {
        let store = PluginStore::open(path).unwrap();
        store.migrate(MIGRATIONS).unwrap();
        Self {
            store: Some(Arc::new(store)),
        }
    }

    pub(crate) fn font_px(&self) -> u32 {
        let Some(store) = &self.store else {
            return DEFAULT_FONT_PX;
        };
        let saved = store.read(|db| {
            db.query_row("SELECT font_px FROM settings WHERE id = 1", [], |row| {
                row.get::<_, i64>(0)
            })
            .optional()
        });
        match saved {
            Ok(Some(px)) => (px.clamp(MIN_FONT_PX as i64, MAX_FONT_PX as i64)) as u32,
            Ok(None) => DEFAULT_FONT_PX,
            Err(error) => {
                eprintln!("[terminal] 读终端字号失败：{error}");
                DEFAULT_FONT_PX
            }
        }
    }

    pub(crate) fn save_font_px(&self, px: u32) {
        let Some(store) = &self.store else {
            return;
        };
        let written = store.write(|db| {
            db.execute(
                "INSERT INTO settings (id, font_px) VALUES (1, ?1)
                 ON CONFLICT (id) DO UPDATE SET font_px = excluded.font_px",
                params![px as i64],
            )
        });
        if let Err(error) = written {
            eprintln!("[terminal] 存终端字号失败：{error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_px_defaults_then_round_trips() {
        let dir =
            std::env::temp_dir().join(format!("smelt-terminal-store-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("store.sqlite3");
        let store = TerminalStore::open_at(&path);
        assert_eq!(store.font_px(), DEFAULT_FONT_PX);
        store.save_font_px(16);
        assert_eq!(store.font_px(), 16);
        store.save_font_px(MAX_FONT_PX + 10);
        assert_eq!(store.font_px(), MAX_FONT_PX, "读出来要夹在可调范围里");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
