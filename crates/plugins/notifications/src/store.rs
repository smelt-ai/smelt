//! 通知插件自己的库（`~/.smelt/plugin-data/smelt.notifications.sqlite3`）：哪类提醒发系统
//! 通知，和发出去、还挂在通知中心里的那些。
//!
//! 都是小量写，在事件里同步写；只在启动时读一次。库打不开（权限、版本比插件新）就只在
//! 内存里用，日志里留一句。

use std::collections::HashSet;
use std::sync::Arc;

use api::model::daemon_state::AttentionKind;
use api::rusqlite::{self, OptionalExtension as _, params};
use api::{PluginStore, RuntimeKey};
use smelt_plugin_api as api;

use crate::OWNER;

const MIGRATIONS: &[&str] = &["
    CREATE TABLE prefs (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        approval INTEGER NOT NULL,
        input INTEGER NOT NULL,
        success INTEGER NOT NULL,
        failure INTEGER NOT NULL,
        bell INTEGER NOT NULL
    );
    CREATE TABLE delivered (
        machine TEXT NOT NULL,
        session_id TEXT NOT NULL,
        PRIMARY KEY (machine, session_id)
    );
"];

/// 哪类提醒发系统通知。默认都发。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Prefs {
    pub(crate) approval: bool,
    pub(crate) input: bool,
    pub(crate) success: bool,
    pub(crate) failure: bool,
    pub(crate) bell: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            approval: true,
            input: true,
            success: true,
            failure: true,
            bell: true,
        }
    }
}

impl Prefs {
    /// 这类提醒发不发。终端通知、智能体发的消息是人家主动要告诉你的，没有开关。
    pub(crate) fn allows(&self, kind: AttentionKind) -> bool {
        match kind {
            AttentionKind::Approval => self.approval,
            AttentionKind::Input => self.input,
            AttentionKind::Success => self.success,
            AttentionKind::Failure => self.failure,
            AttentionKind::Bell => self.bell,
            AttentionKind::Notice | AttentionKind::Unknown => true,
        }
    }
}

/// 发出去、还挂在通知中心里的一条：哪台机器的哪段会话。
pub(crate) type Delivered = (RuntimeKey, String);

pub(crate) struct NotifyStore {
    store: Option<Arc<PluginStore>>,
}

impl NotifyStore {
    /// 打开本插件的库。打不开就是个什么都不记的空壳。
    pub(crate) fn open() -> Self {
        let store = api::plugin_store(OWNER)
            .map_err(|error| error.to_string())
            .and_then(|store| Self::migrate(&store).map(|()| store));
        match store {
            Ok(store) => Self::with(store),
            Err(error) => {
                eprintln!("[notifications] 库打不开，设置不会保存：{error}");
                Self { store: None }
            }
        }
    }

    pub(crate) fn with(store: Arc<PluginStore>) -> Self {
        Self { store: Some(store) }
    }

    pub(crate) fn migrate(store: &PluginStore) -> Result<(), String> {
        store.migrate(MIGRATIONS).map_err(|error| error.to_string())
    }

    pub(crate) fn prefs(&self) -> Prefs {
        self.read(|connection| {
            connection
                .query_row(
                    "SELECT approval, input, success, failure, bell FROM prefs WHERE id = 1",
                    [],
                    |row| {
                        Ok(Prefs {
                            approval: row.get(0)?,
                            input: row.get(1)?,
                            success: row.get(2)?,
                            failure: row.get(3)?,
                            bell: row.get(4)?,
                        })
                    },
                )
                .optional()
        })
        .flatten()
        .unwrap_or_default()
    }

    pub(crate) fn save_prefs(&self, prefs: Prefs) {
        self.write(|transaction| {
            transaction.execute(
                "INSERT INTO prefs (id, approval, input, success, failure, bell)
                 VALUES (1, ?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (id) DO UPDATE SET approval = ?1, input = ?2, success = ?3,
                     failure = ?4, bell = ?5",
                params![
                    prefs.approval,
                    prefs.input,
                    prefs.success,
                    prefs.failure,
                    prefs.bell
                ],
            )
        });
    }

    pub(crate) fn delivered(&self) -> HashSet<Delivered> {
        self.read(|connection| {
            let mut statement = connection.prepare("SELECT machine, session_id FROM delivered")?;
            let rows = statement.query_map([], |row| {
                Ok((machine_key(row.get(0)?), row.get::<_, String>(1)?))
            })?;
            rows.collect::<rusqlite::Result<HashSet<_>>>()
        })
        .unwrap_or_default()
    }

    pub(crate) fn add_delivered(&self, (machine, session_id): &Delivered) {
        self.write(|transaction| {
            transaction.execute(
                "INSERT OR IGNORE INTO delivered (machine, session_id) VALUES (?1, ?2)",
                params![machine_column(machine), session_id],
            )
        });
    }

    pub(crate) fn remove_delivered(&self, (machine, session_id): &Delivered) {
        self.write(|transaction| {
            transaction.execute(
                "DELETE FROM delivered WHERE machine = ?1 AND session_id = ?2",
                params![machine_column(machine), session_id],
            )
        });
    }

    fn read<T>(&self, run: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T>) -> Option<T> {
        let store = self.store.as_ref()?;
        store
            .read(run)
            .inspect_err(|error| eprintln!("[notifications] 读库失败：{error}"))
            .ok()
    }

    fn write(&self, run: impl FnOnce(&rusqlite::Transaction) -> rusqlite::Result<usize>) {
        if let Some(store) = &self.store
            && let Err(error) = store.write(run)
        {
            eprintln!("[notifications] 写库失败：{error}");
        }
    }
}

/// 本机存成空串，远程机器存它的 runtime id。
fn machine_column(machine: &RuntimeKey) -> &str {
    match machine {
        RuntimeKey::Local => "",
        RuntimeKey::Remote(id) => id,
    }
}

fn machine_key(column: String) -> RuntimeKey {
    if column.is_empty() {
        RuntimeKey::Local
    } else {
        RuntimeKey::Remote(column)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, NotifyStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(PluginStore::open(dir.path().join("notify.sqlite3")).unwrap());
        NotifyStore::migrate(&store).unwrap();
        (dir, NotifyStore::with(store))
    }

    #[test]
    fn prefs_default_to_on_and_round_trip() {
        let (_dir, store) = store();
        assert_eq!(store.prefs(), Prefs::default());
        let prefs = Prefs {
            success: false,
            bell: false,
            ..Prefs::default()
        };
        store.save_prefs(prefs);
        assert_eq!(store.prefs(), prefs);
    }

    #[test]
    fn delivered_keeps_machines_apart() {
        let (_dir, store) = store();
        let local = (RuntimeKey::Local, "a".to_string());
        let remote = (RuntimeKey::Remote("rt".to_string()), "a".to_string());
        store.add_delivered(&local);
        store.add_delivered(&remote);
        store.add_delivered(&remote);
        assert_eq!(
            store.delivered(),
            HashSet::from([local.clone(), remote.clone()])
        );
        store.remove_delivered(&local);
        assert_eq!(store.delivered(), HashSet::from([remote]));
    }

    /// 终端通知、智能体的消息没有开关；新运行时报的认不出的类型按通知算。
    #[test]
    fn notices_cannot_be_switched_off() {
        let prefs = Prefs {
            approval: false,
            input: false,
            success: false,
            failure: false,
            bell: false,
        };
        assert!(!prefs.allows(AttentionKind::Bell));
        assert!(prefs.allows(AttentionKind::Notice));
        assert!(prefs.allows(AttentionKind::Unknown));
    }
}
