//! 离线缓存：断网或运行时离线时，界面先显示上次看到的会话列表和最近几个对话的尾部。
//!
//! 存在 App 数据目录的 SQLite 里，按运行时（endpoint id）分开。缓存的是从运行时收到的数据
//! 本身（会话列表、对话快照的线格式），读回来走和在线时同一条转换路径，不另存一套视图模型。
//! 这只是缓存：库坏了或版本不认识就整库重建，不做迁移。

use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

pub(crate) const DATABASE_FILE: &str = "smelt-mobile-cache.sqlite3";

/// 库结构版本。改表就加一，旧库直接删掉重建。
const SCHEMA_VERSION: i64 = 1;

/// 每个运行时最多留几个对话的快照、合计多大。按最近保存时间淘汰。
pub(crate) const MAX_SNAPSHOTS_PER_RUNTIME: usize = 5;
pub(crate) const MAX_SNAPSHOT_BYTES_PER_RUNTIME: usize = 32 * 1024 * 1024;

const SCHEMA: &str = "
CREATE TABLE session_list (
    endpoint_id TEXT PRIMARY KEY,
    saved_at_ms INTEGER NOT NULL,
    payload     TEXT NOT NULL
);
CREATE TABLE conversation_snapshot (
    endpoint_id TEXT NOT NULL,
    session_id  TEXT NOT NULL,
    saved_at_ms INTEGER NOT NULL,
    payload     TEXT NOT NULL,
    PRIMARY KEY (endpoint_id, session_id)
);
";

/// 从缓存读回的一份数据和它的保存时间。
#[derive(Debug)]
pub(crate) struct Cached<T> {
    pub(crate) value: T,
    pub(crate) saved_at_ms: i64,
}

pub(crate) struct Cache {
    conn: Mutex<Connection>,
}

impl Cache {
    /// 打开（必要时新建）缓存库。版本不对或打不开就删掉重建：缓存丢了只是少显示一点旧数据。
    pub(crate) fn open(path: &Path) -> rusqlite::Result<Self> {
        match Self::open_existing(path) {
            Ok(cache) => Ok(cache),
            Err(error) => {
                log::warn!("离线缓存不可用，重建：{error}");
                let _ = std::fs::remove_file(path);
                Self::open_existing(path)
            }
        }
    }

    /// 测试用：内存库。
    #[cfg(test)]
    pub(crate) fn in_memory() -> Self {
        let conn = Connection::open_in_memory().expect("内存库");
        conn.execute_batch(SCHEMA).expect("建表");
        Self {
            conn: Mutex::new(conn),
        }
    }

    fn open_existing(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version != SCHEMA_VERSION {
            if version != 0 {
                return Err(rusqlite::Error::InvalidQuery);
            }
            conn.execute_batch(SCHEMA)?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn save_session_list<T: Serialize>(&self, endpoint_id: &str, value: &T) {
        let Ok(payload) = serde_json::to_string(value) else {
            return;
        };
        let result = self.conn().execute(
            "INSERT INTO session_list (endpoint_id, saved_at_ms, payload) VALUES (?1, ?2, ?3)
             ON CONFLICT(endpoint_id) DO UPDATE SET saved_at_ms = ?2, payload = ?3",
            params![endpoint_id, now_ms(), payload],
        );
        if let Err(error) = result {
            log::warn!("保存会话列表缓存失败：{error}");
        }
    }

    pub(crate) fn load_session_list<T: DeserializeOwned>(
        &self,
        endpoint_id: &str,
    ) -> Option<Cached<T>> {
        let row: Option<(i64, String)> = self
            .conn()
            .query_row(
                "SELECT saved_at_ms, payload FROM session_list WHERE endpoint_id = ?1",
                params![endpoint_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .ok()
            .flatten();
        let (saved_at_ms, payload) = row?;
        let value = serde_json::from_str(&payload).ok()?;
        Some(Cached { value, saved_at_ms })
    }

    /// 存一个对话的快照，并按条数和总大小淘汰这个运行时最旧的快照。
    pub(crate) fn save_snapshot<T: Serialize>(
        &self,
        endpoint_id: &str,
        session_id: &str,
        value: &T,
    ) {
        let Ok(payload) = serde_json::to_string(value) else {
            return;
        };
        let mut conn = self.conn();
        let result = (|| -> rusqlite::Result<()> {
            let tx = conn.transaction()?;
            tx.execute(
                "INSERT INTO conversation_snapshot (endpoint_id, session_id, saved_at_ms, payload)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(endpoint_id, session_id) DO UPDATE SET saved_at_ms = ?3, payload = ?4",
                params![endpoint_id, session_id, now_ms(), payload],
            )?;
            prune_snapshots(&tx, endpoint_id)?;
            tx.commit()
        })();
        if let Err(error) = result {
            log::warn!("保存对话快照缓存失败：{error}");
        }
    }

    pub(crate) fn load_snapshot<T: DeserializeOwned>(
        &self,
        endpoint_id: &str,
        session_id: &str,
    ) -> Option<Cached<T>> {
        let row: Option<(i64, String)> = self
            .conn()
            .query_row(
                "SELECT saved_at_ms, payload FROM conversation_snapshot
                 WHERE endpoint_id = ?1 AND session_id = ?2",
                params![endpoint_id, session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .ok()
            .flatten();
        let (saved_at_ms, payload) = row?;
        let value = serde_json::from_str(&payload).ok()?;
        Some(Cached { value, saved_at_ms })
    }

    /// 删掉一台机器的全部缓存（忘记这台机器时）。
    pub(crate) fn delete_runtime(&self, endpoint_id: &str) {
        for table in ["session_list", "conversation_snapshot"] {
            let result = self.conn().execute(
                &format!("DELETE FROM {table} WHERE endpoint_id = ?1"),
                params![endpoint_id],
            );
            if let Err(error) = result {
                log::warn!("删除机器的缓存失败：{error}");
            }
        }
    }

    pub(crate) fn delete_snapshot(&self, endpoint_id: &str, session_id: &str) {
        let result = self.conn().execute(
            "DELETE FROM conversation_snapshot WHERE endpoint_id = ?1 AND session_id = ?2",
            params![endpoint_id, session_id],
        );
        if let Err(error) = result {
            log::warn!("删除对话快照缓存失败：{error}");
        }
    }
}

/// 留最近保存的若干个快照，且合计不超过上限；最新的那个总会留下。
fn prune_snapshots(conn: &Connection, endpoint_id: &str) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(
        "SELECT session_id, length(payload) FROM conversation_snapshot
         WHERE endpoint_id = ?1 ORDER BY saved_at_ms DESC, rowid DESC",
    )?;
    let rows = stmt
        .query_map(params![endpoint_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut kept = 0usize;
    let mut bytes = 0usize;
    for (session_id, len) in rows {
        let len = usize::try_from(len).unwrap_or(usize::MAX);
        let fits = kept < MAX_SNAPSHOTS_PER_RUNTIME
            && (kept == 0 || bytes.saturating_add(len) <= MAX_SNAPSHOT_BYTES_PER_RUNTIME);
        if fits {
            kept += 1;
            bytes = bytes.saturating_add(len);
        } else {
            conn.execute(
                "DELETE FROM conversation_snapshot WHERE endpoint_id = ?1 AND session_id = ?2",
                params![endpoint_id, session_id],
            )?;
        }
    }
    Ok(())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_list_roundtrips_per_runtime() {
        let cache = Cache::in_memory();
        cache.save_session_list("a", &vec!["s1".to_string()]);
        cache.save_session_list("b", &vec!["s2".to_string()]);
        let a: Cached<Vec<String>> = cache.load_session_list("a").unwrap();
        assert_eq!(a.value, vec!["s1"]);
        assert!(a.saved_at_ms > 0);
        assert!(cache.load_session_list::<Vec<String>>("c").is_none());
    }

    #[test]
    fn snapshots_are_pruned_to_the_most_recent() {
        let cache = Cache::in_memory();
        for i in 0..MAX_SNAPSHOTS_PER_RUNTIME + 2 {
            cache.save_snapshot("a", &format!("s{i}"), &i);
        }
        cache.save_snapshot("b", "other", &0);
        let kept = (0..MAX_SNAPSHOTS_PER_RUNTIME + 2)
            .filter(|i| {
                cache
                    .load_snapshot::<usize>("a", &format!("s{i}"))
                    .is_some()
            })
            .count();
        assert_eq!(kept, MAX_SNAPSHOTS_PER_RUNTIME);
        assert!(
            cache.load_snapshot::<usize>("a", "s0").is_none(),
            "最旧的先淘汰"
        );
        assert!(
            cache
                .load_snapshot::<usize>("a", &format!("s{}", MAX_SNAPSHOTS_PER_RUNTIME + 1))
                .is_some()
        );
        assert!(
            cache.load_snapshot::<usize>("b", "other").is_some(),
            "别的运行时不受影响"
        );
    }

    #[test]
    fn deleting_a_runtime_drops_only_its_rows() {
        let cache = Cache::in_memory();
        cache.save_session_list("a", &1);
        cache.save_snapshot("a", "s", &1);
        cache.save_session_list("b", &2);
        cache.save_snapshot("b", "s", &2);
        cache.delete_runtime("a");
        assert!(cache.load_session_list::<i32>("a").is_none());
        assert!(cache.load_snapshot::<i32>("a", "s").is_none());
        assert_eq!(cache.load_session_list::<i32>("b").unwrap().value, 2);
        assert_eq!(cache.load_snapshot::<i32>("b", "s").unwrap().value, 2);
    }

    #[test]
    fn oversized_snapshot_still_keeps_the_newest() {
        let cache = Cache::in_memory();
        let big = "x".repeat(MAX_SNAPSHOT_BYTES_PER_RUNTIME + 1);
        cache.save_snapshot("a", "old", &"small");
        cache.save_snapshot("a", "big", &big);
        assert!(cache.load_snapshot::<String>("a", "big").is_some());
        assert!(cache.load_snapshot::<String>("a", "old").is_none());
    }

    #[test]
    fn unknown_schema_version_is_rebuilt() {
        let dir = std::env::temp_dir().join(format!("smelt-mobile-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(DATABASE_FILE);
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "user_version", 99).unwrap();
        }
        let cache = Cache::open(&path).unwrap();
        cache.save_session_list("a", &1);
        assert_eq!(cache.load_session_list::<i32>("a").unwrap().value, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
