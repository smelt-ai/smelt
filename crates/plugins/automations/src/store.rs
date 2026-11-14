//! 自动化插件自己的库（`~/.smelt/plugin-data/smelt.automations.sqlite3`）：发出去、还挂在
//! 通知中心里的运行完成通知。记下来是为了运行记录被清掉、机器移除之后撤得掉，重启后也撤得掉。
//!
//! 小量写，在事件里同步写；只在启动时读一次。库打不开就只在内存里记，日志里留一句。

use std::collections::HashSet;
use std::sync::Arc;

use api::rusqlite::{self, params};
use api::{PluginStore, RuntimeKey};
use smelt_plugin_api as api;

use crate::OWNER;

const MIGRATIONS: &[&str] = &["
    CREATE TABLE delivered (
        machine TEXT NOT NULL,
        run_id TEXT NOT NULL,
        PRIMARY KEY (machine, run_id)
    );
"];

/// 发出去的一条：哪台机器上的哪次运行。
pub(crate) type Delivered = (RuntimeKey, String);

pub(crate) struct DeliveredStore {
    store: Option<Arc<PluginStore>>,
}

impl DeliveredStore {
    /// 打开本插件的库。打不开就是个什么都不记的空壳。
    pub(crate) fn open() -> Self {
        let store = api::plugin_store(OWNER)
            .map_err(|error| error.to_string())
            .and_then(|store| Self::migrate(&store).map(|()| store));
        match store {
            Ok(store) => Self::with(store),
            Err(error) => {
                eprintln!("[automations] 库打不开，发过的通知重启后撤不掉：{error}");
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

    pub(crate) fn delivered(&self) -> HashSet<Delivered> {
        let Some(store) = &self.store else {
            return HashSet::new();
        };
        store
            .read(|connection| {
                let mut statement = connection.prepare("SELECT machine, run_id FROM delivered")?;
                let rows = statement.query_map([], |row| {
                    Ok((machine_key(row.get(0)?), row.get::<_, String>(1)?))
                })?;
                rows.collect::<rusqlite::Result<HashSet<_>>>()
            })
            .inspect_err(|error| eprintln!("[automations] 读库失败：{error}"))
            .unwrap_or_default()
    }

    pub(crate) fn add(&self, (machine, run_id): &Delivered) {
        self.write(|transaction| {
            transaction.execute(
                "INSERT OR IGNORE INTO delivered (machine, run_id) VALUES (?1, ?2)",
                params![machine_column(machine), run_id],
            )
        });
    }

    pub(crate) fn remove(&self, (machine, run_id): &Delivered) {
        self.write(|transaction| {
            transaction.execute(
                "DELETE FROM delivered WHERE machine = ?1 AND run_id = ?2",
                params![machine_column(machine), run_id],
            )
        });
    }

    fn write(&self, run: impl FnOnce(&rusqlite::Transaction) -> rusqlite::Result<usize>) {
        if let Some(store) = &self.store
            && let Err(error) = store.write(run)
        {
            eprintln!("[automations] 写库失败：{error}");
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

    #[test]
    fn delivered_keeps_machines_apart() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(PluginStore::open(dir.path().join("automations.sqlite3")).unwrap());
        DeliveredStore::migrate(&store).unwrap();
        let store = DeliveredStore::with(store);
        let local = (RuntimeKey::Local, "run".to_string());
        let remote = (RuntimeKey::Remote("rt".to_string()), "run".to_string());
        store.add(&local);
        store.add(&remote);
        store.add(&remote);
        assert_eq!(
            store.delivered(),
            HashSet::from([local.clone(), remote.clone()])
        );
        store.remove(&local);
        assert_eq!(store.delivered(), HashSet::from([remote]));
    }
}
