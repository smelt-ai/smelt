//! 语言偏好数据库（`~/.smelt/plugin-data/smelt.language/store.sqlite3`）：用户选的界面语言。

use std::sync::Arc;

use smelt_plugin_api::rusqlite::{OptionalExtension as _, params};
use smelt_plugin_api::{self as api, PluginStore, plugin_store};

use super::OWNER;

/// `locale` 为 NULL 是跟着系统。
const MIGRATIONS: &[&str] = &["
    CREATE TABLE settings (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        locale TEXT
    );
"];

pub(crate) struct LanguageStore {
    store: Option<Arc<PluginStore>>,
}

impl LanguageStore {
    pub(crate) fn open() -> Self {
        let store = plugin_store(OWNER).and_then(|store| {
            store.migrate(MIGRATIONS)?;
            Ok(store)
        });
        match store {
            Ok(store) => Self { store: Some(store) },
            Err(error) => {
                eprintln!("[language] 打不开语言的库，选的语言这次不记：{error}");
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

    pub(crate) fn choice(&self) -> Option<&'static str> {
        let store = self.store.as_ref()?;
        let read = store.read(|db| {
            db.query_row("SELECT locale FROM settings WHERE id = 1", [], |row| {
                row.get::<_, Option<String>>(0)
            })
            .optional()
        });
        let saved = match read {
            Ok(saved) => saved.flatten()?,
            Err(error) => {
                eprintln!("[language] 读选的语言失败，跟着系统：{error}");
                return None;
            }
        };
        api::LOCALES
            .iter()
            .find(|(code, _)| *code == saved)
            .map(|(code, _)| *code)
    }

    pub(crate) fn save(&self, choice: Option<&'static str>) {
        let Some(store) = &self.store else {
            return;
        };
        let written = store.write(|db| {
            db.execute(
                "INSERT INTO settings (id, locale) VALUES (1, ?1)
                 ON CONFLICT (id) DO UPDATE SET locale = excluded.locale",
                params![choice],
            )
        });
        if let Err(error) = written {
            eprintln!("[language] 记选的语言失败，这次运行里照样生效：{error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choice_round_trips_and_unknown_codes_follow_the_system() {
        let dir = std::env::temp_dir().join(format!("smelt-language-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("store.sqlite3");
        let store = LanguageStore::open_at(&path);
        assert_eq!(store.choice(), None, "没选过是跟随系统");
        store.save(Some("zh-CN"));
        assert_eq!(store.choice(), Some("zh-CN"));
        store.save(None);
        assert_eq!(store.choice(), None);
        store.save(Some("zh-CN"));
        store
            .store
            .as_ref()
            .unwrap()
            .write(|db| db.execute("UPDATE settings SET locale = 'tlh' WHERE id = 1", []))
            .unwrap();
        assert_eq!(store.choice(), None, "这一版不带的语言按跟随系统");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
