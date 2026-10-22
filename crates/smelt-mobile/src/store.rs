//! 本机业务数据：登记过的机器、消息草稿。
//!
//! 存在 App 数据目录的 `smelt-mobile.sqlite3`。和离线缓存（`crate::cache`）不同，这是用户
//! 自己的数据：库结构用 `PRAGMA user_version` 记版本，升级时按 [`MIGRATIONS`] 一步步迁，
//! 绝不删库；遇到比这个版本还新的库（装回了旧版 App）就报错，不去动它。

use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::api::conversation::ImageInput;
use crate::api::draft::MessageDraft;
use crate::api::error::{SmeltError, SmeltErrorKind, error};
use crate::api::fleet::RuntimeRecord;
use crate::connection::RuntimeInfo;

pub(crate) const DATABASE_FILE: &str = "smelt-mobile.sqlite3";

/// 灶台标签色板的大小。序号 `0..ACCENT_COUNT`，颜色由界面定。
pub(crate) const ACCENT_COUNT: u32 = 8;

/// 第 n 项把库从版本 n 迁到 n+1。只追加，不改已发布的步骤。
const MIGRATIONS: &[&str] = &["
CREATE TABLE runtime (
    runtime_id   TEXT PRIMARY KEY,
    endpoint_id  TEXT NOT NULL UNIQUE,
    relay_url    TEXT,
    name         TEXT NOT NULL,
    hostname     TEXT,
    os           TEXT,
    version      TEXT NOT NULL,
    accent       INTEGER NOT NULL,
    position     INTEGER NOT NULL,
    paired_at_ms INTEGER NOT NULL
);
CREATE TABLE draft (
    runtime_id    TEXT NOT NULL,
    session_id    TEXT NOT NULL,
    content       TEXT NOT NULL,
    request_id    TEXT,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (runtime_id, session_id)
);
CREATE TABLE draft_image (
    runtime_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    position   INTEGER NOT NULL,
    mime       TEXT NOT NULL,
    bytes      BLOB NOT NULL,
    PRIMARY KEY (runtime_id, session_id, position)
);
"];

const RUNTIME_COLUMNS: &str = "runtime_id, name, endpoint_id, relay_url, hostname, os, version, \
                               accent, position, paired_at_ms";

/// 配对写登记的结果。
#[derive(Debug)]
pub(crate) struct Paired {
    pub record: RuntimeRecord,
    /// 这个 endpoint 原先登记在别的 `runtime_id` 下（那台机器重装后换了身份）：旧登记连同
    /// 草稿已删，调用方要撤掉它的节点。
    pub replaced: Option<String>,
}

pub(crate) struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    /// 打开（必要时新建）并迁到最新版本。
    pub(crate) fn open(path: &Path) -> Result<Self, SmeltError> {
        let mut conn = Connection::open(path).map_err(db_error)?;
        migrate(&mut conn, MIGRATIONS).map_err(db_error)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// 测试用：内存库。
    #[cfg(test)]
    pub(crate) fn in_memory() -> Self {
        let mut conn = Connection::open_in_memory().expect("内存库");
        migrate(&mut conn, MIGRATIONS).expect("建表");
        Self {
            conn: Mutex::new(conn),
        }
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 在一个事务里做完 `f`。
    fn write<T>(
        &self,
        f: impl FnOnce(&Transaction<'_>) -> Result<T, SmeltError>,
    ) -> Result<T, SmeltError> {
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(db_error)?;
        let value = f(&tx)?;
        tx.commit().map_err(db_error)?;
        Ok(value)
    }

    // -----------------------------------------------------------------------
    // 机器
    // -----------------------------------------------------------------------

    /// 全部登记，按列表顺序。
    pub(crate) fn runtimes(&self) -> Result<Vec<RuntimeRecord>, SmeltError> {
        runtimes(&self.conn()).map_err(db_error)
    }

    /// 配对成功后写登记。已登记的机器（同一 `runtime_id`）更新地址和描述，名字、标签色、
    /// 顺序不变；新机器名字取主机名，标签色取色板里用得最少的最小序号，排在最后。
    pub(crate) fn record_pairing(
        &self,
        info: &RuntimeInfo,
        paired_at_ms: i64,
    ) -> Result<Paired, SmeltError> {
        self.write(|tx| {
            let replaced: Option<String> = tx
                .query_row(
                    "SELECT runtime_id FROM runtime WHERE endpoint_id = ?1 AND runtime_id != ?2",
                    params![info.endpoint_id, info.runtime_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(db_error)?;
            if let Some(old) = &replaced {
                delete_runtime_rows(tx, old)?;
                // 补上空出来的序号，下面新插的排在最后时才不会和别人重号。
                let ids = ordered_ids(tx)?;
                renumber(tx, &ids)?;
            }
            let updated = tx
                .execute(
                    "UPDATE runtime SET endpoint_id = ?2, relay_url = ?3, hostname = ?4, os = ?5,
                         version = ?6, paired_at_ms = ?7
                     WHERE runtime_id = ?1",
                    params![
                        info.runtime_id,
                        info.endpoint_id,
                        info.relay_url,
                        info.hostname,
                        info.os,
                        info.version,
                        paired_at_ms
                    ],
                )
                .map_err(db_error)?;
            if updated == 0 {
                insert_runtime(
                    tx,
                    &NewRuntime {
                        runtime_id: &info.runtime_id,
                        name: &default_name(info.hostname.as_deref(), &info.endpoint_id),
                        endpoint_id: &info.endpoint_id,
                        relay_url: info.relay_url.as_deref(),
                        hostname: info.hostname.as_deref(),
                        os: info.os.as_deref(),
                        version: &info.version,
                        paired_at_ms,
                    },
                )?;
            }
            let record = runtime(tx, &info.runtime_id)?;
            Ok(Paired { record, replaced })
        })
    }

    /// 导入旧版存的一台机器。`runtime_id` 或 endpoint 已登记就跳过，返回 None。
    pub(crate) fn import_runtime(
        &self,
        runtime_id: &str,
        endpoint_id: &str,
        relay_url: Option<&str>,
        name: &str,
        paired_at_ms: i64,
    ) -> Result<Option<RuntimeRecord>, SmeltError> {
        self.write(|tx| {
            let exists: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM runtime WHERE runtime_id = ?1 OR endpoint_id = ?2)",
                    params![runtime_id, endpoint_id],
                    |row| row.get(0),
                )
                .map_err(db_error)?;
            if exists {
                return Ok(None);
            }
            let name = name.trim();
            let name = if name.is_empty() {
                default_name(None, endpoint_id)
            } else {
                name.to_string()
            };
            insert_runtime(
                tx,
                &NewRuntime {
                    runtime_id,
                    name: &name,
                    endpoint_id,
                    relay_url,
                    hostname: None,
                    os: None,
                    // 还没连上过，不知道版本；连上后由 `record_describe` 补上。
                    version: "",
                    paired_at_ms,
                },
            )?;
            runtime(tx, runtime_id).map(Some)
        })
    }

    /// 连上后用 `system.describe` 的结果刷新主机名、系统和版本。有变化时返回新的登记。
    pub(crate) fn record_describe(
        &self,
        info: &RuntimeInfo,
    ) -> Result<Option<RuntimeRecord>, SmeltError> {
        self.write(|tx| {
            let changed = tx
                .execute(
                    "UPDATE runtime SET hostname = ?2, os = ?3, version = ?4
                     WHERE runtime_id = ?1
                       AND (hostname IS NOT ?2 OR os IS NOT ?3 OR version IS NOT ?4)",
                    params![info.runtime_id, info.hostname, info.os, info.version],
                )
                .map_err(db_error)?;
            if changed == 0 {
                return Ok(None);
            }
            runtime(tx, &info.runtime_id).map(Some)
        })
    }

    pub(crate) fn rename(&self, runtime_id: &str, name: &str) -> Result<(), SmeltError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(error(SmeltErrorKind::Other, "名字不能为空"));
        }
        self.update_one(
            "UPDATE runtime SET name = ?2 WHERE runtime_id = ?1",
            params![runtime_id, name],
            runtime_id,
        )
    }

    pub(crate) fn set_accent(&self, runtime_id: &str, accent: u32) -> Result<(), SmeltError> {
        if accent >= ACCENT_COUNT {
            return Err(error(
                SmeltErrorKind::Other,
                format!("标签色序号要在 0..{ACCENT_COUNT} 之间"),
            ));
        }
        self.update_one(
            "UPDATE runtime SET accent = ?2 WHERE runtime_id = ?1",
            params![runtime_id, accent],
            runtime_id,
        )
    }

    fn update_one(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
        runtime_id: &str,
    ) -> Result<(), SmeltError> {
        match self.conn().execute(sql, params).map_err(db_error)? {
            0 => Err(unknown_runtime(runtime_id)),
            _ => Ok(()),
        }
    }

    /// 挪到第 `to_index` 位（超出就放末尾），重排后顺序从 0 起连续。
    pub(crate) fn move_runtime(&self, runtime_id: &str, to_index: u32) -> Result<(), SmeltError> {
        self.write(|tx| {
            let mut ids = ordered_ids(tx)?;
            let from = ids
                .iter()
                .position(|id| id == runtime_id)
                .ok_or_else(|| unknown_runtime(runtime_id))?;
            let id = ids.remove(from);
            let to = (to_index as usize).min(ids.len());
            ids.insert(to, id);
            renumber(tx, &ids)
        })
    }

    /// 删掉登记和它的全部草稿。没登记过返回 false。
    pub(crate) fn delete_runtime(&self, runtime_id: &str) -> Result<bool, SmeltError> {
        self.write(|tx| {
            let deleted = delete_runtime_rows(tx, runtime_id)?;
            let ids = ordered_ids(tx)?;
            renumber(tx, &ids)?;
            Ok(deleted)
        })
    }

    // -----------------------------------------------------------------------
    // 草稿
    // -----------------------------------------------------------------------

    pub(crate) fn load_draft(
        &self,
        runtime_id: &str,
        session_id: &str,
    ) -> Result<Option<MessageDraft>, SmeltError> {
        let conn = self.conn();
        let row: Option<(String, Option<String>)> = conn
            .query_row(
                "SELECT content, request_id FROM draft WHERE runtime_id = ?1 AND session_id = ?2",
                params![runtime_id, session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(db_error)?;
        let Some((content, request_id)) = row else {
            return Ok(None);
        };
        let mut stmt = conn
            .prepare(
                "SELECT mime, bytes FROM draft_image WHERE runtime_id = ?1 AND session_id = ?2
                 ORDER BY position",
            )
            .map_err(db_error)?;
        let images = stmt
            .query_map(params![runtime_id, session_id], |row| {
                Ok(ImageInput {
                    mime: row.get(0)?,
                    bytes: row.get(1)?,
                })
            })
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
            .map_err(db_error)?;
        Ok(Some(MessageDraft {
            content,
            images,
            request_id,
        }))
    }

    /// 存草稿，整份替换。空草稿（没有文字也没有图）等于删掉。机器没登记时报错，免得忘记
    /// 机器之后还留着它的草稿。
    pub(crate) fn save_draft(
        &self,
        runtime_id: &str,
        session_id: &str,
        draft: &MessageDraft,
        updated_at_ms: i64,
    ) -> Result<(), SmeltError> {
        if draft.content.trim().is_empty() && draft.images.is_empty() {
            return self.clear_draft(runtime_id, session_id);
        }
        self.write(|tx| {
            let registered: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM runtime WHERE runtime_id = ?1)",
                    params![runtime_id],
                    |row| row.get(0),
                )
                .map_err(db_error)?;
            if !registered {
                return Err(unknown_runtime(runtime_id));
            }
            tx.execute(
                "INSERT INTO draft (runtime_id, session_id, content, request_id, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(runtime_id, session_id) DO UPDATE
                 SET content = ?3, request_id = ?4, updated_at_ms = ?5",
                params![
                    runtime_id,
                    session_id,
                    draft.content,
                    draft.request_id,
                    updated_at_ms
                ],
            )
            .map_err(db_error)?;
            tx.execute(
                "DELETE FROM draft_image WHERE runtime_id = ?1 AND session_id = ?2",
                params![runtime_id, session_id],
            )
            .map_err(db_error)?;
            for (position, image) in draft.images.iter().enumerate() {
                tx.execute(
                    "INSERT INTO draft_image (runtime_id, session_id, position, mime, bytes)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        runtime_id,
                        session_id,
                        position as i64,
                        image.mime,
                        image.bytes
                    ],
                )
                .map_err(db_error)?;
            }
            Ok(())
        })
    }

    pub(crate) fn clear_draft(&self, runtime_id: &str, session_id: &str) -> Result<(), SmeltError> {
        self.write(|tx| delete_draft_rows(tx, runtime_id, session_id))
    }

    /// 一次发送有了结果：存着的草稿还是这次发送的（`request_id` 对得上）才处理——成功就删，
    /// 失败就清掉 `request_id`（下次发是新的提交）。发送途中用户又改了草稿就不动，返回 false。
    pub(crate) fn resolve_draft(
        &self,
        runtime_id: &str,
        session_id: &str,
        request_id: &str,
        succeeded: bool,
    ) -> Result<bool, SmeltError> {
        self.write(|tx| {
            let stored: Option<Option<String>> = tx
                .query_row(
                    "SELECT request_id FROM draft WHERE runtime_id = ?1 AND session_id = ?2",
                    params![runtime_id, session_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(db_error)?;
            if stored.flatten().as_deref() != Some(request_id) {
                return Ok(false);
            }
            if succeeded {
                delete_draft_rows(tx, runtime_id, session_id)?;
            } else {
                tx.execute(
                    "UPDATE draft SET request_id = NULL WHERE runtime_id = ?1 AND session_id = ?2",
                    params![runtime_id, session_id],
                )
                .map_err(db_error)?;
            }
            Ok(true)
        })
    }
}

/// 从库里记的版本迁到 `steps.len()`。每步一个事务，版本号随步骤一起提交：中途失败停在
/// 上一步，下次启动接着迁。
fn migrate(conn: &mut Connection, steps: &[&str]) -> rusqlite::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let version = usize::try_from(version).unwrap_or(usize::MAX);
    if version > steps.len() {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISMATCH),
            Some(format!(
                "本机数据库版本 {version} 比这个 App 认识的 {} 新，先升级 App",
                steps.len()
            )),
        ));
    }
    for (index, step) in steps.iter().enumerate().skip(version) {
        let tx = conn.transaction()?;
        tx.execute_batch(step)?;
        tx.pragma_update(None, "user_version", (index + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}

struct NewRuntime<'a> {
    runtime_id: &'a str,
    name: &'a str,
    endpoint_id: &'a str,
    relay_url: Option<&'a str>,
    hostname: Option<&'a str>,
    os: Option<&'a str>,
    version: &'a str,
    paired_at_ms: i64,
}

/// 新登记：标签色取用得最少的最小序号（没用满时就是没用过的最小序号），排在最后。
fn insert_runtime(tx: &Transaction<'_>, new: &NewRuntime<'_>) -> Result<(), SmeltError> {
    let mut used = Vec::new();
    {
        let mut stmt = tx.prepare("SELECT accent FROM runtime").map_err(db_error)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, i64>(0))
            .map_err(db_error)?;
        for accent in rows {
            used.push(accent.map_err(db_error)?);
        }
    }
    let accent = next_accent(&used);
    let position: i64 = tx
        .query_row("SELECT COUNT(*) FROM runtime", [], |row| row.get(0))
        .map_err(db_error)?;
    tx.execute(
        "INSERT INTO runtime (runtime_id, name, endpoint_id, relay_url, hostname, os, version,
             accent, position, paired_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            new.runtime_id,
            new.name,
            new.endpoint_id,
            new.relay_url,
            new.hostname,
            new.os,
            new.version,
            accent,
            position,
            new.paired_at_ms
        ],
    )
    .map_err(db_error)?;
    Ok(())
}

/// 色板里用得最少的序号中最小的一个。
pub(crate) fn next_accent(used: &[i64]) -> u32 {
    (0..ACCENT_COUNT)
        .min_by_key(|accent| {
            let count = used.iter().filter(|used| **used == *accent as i64).count();
            (count, *accent)
        })
        .unwrap_or(0)
}

/// 默认名字：主机名，没有就用 endpoint 前缀。
pub(crate) fn default_name(hostname: Option<&str>, endpoint_id: &str) -> String {
    if let Some(hostname) = hostname.map(str::trim).filter(|name| !name.is_empty()) {
        return hostname.to_string();
    }
    let prefix: String = endpoint_id.chars().take(8).collect();
    if prefix.is_empty() {
        "Runtime".to_string()
    } else {
        format!("Runtime {prefix}")
    }
}

fn read_runtime(row: &rusqlite::Row<'_>) -> rusqlite::Result<RuntimeRecord> {
    Ok(RuntimeRecord {
        runtime_id: row.get(0)?,
        name: row.get(1)?,
        endpoint_id: row.get(2)?,
        relay_url: row.get(3)?,
        hostname: row.get(4)?,
        os: row.get(5)?,
        version: row.get(6)?,
        accent: u32::try_from(row.get::<_, i64>(7)?).unwrap_or(0),
        position: u32::try_from(row.get::<_, i64>(8)?).unwrap_or(u32::MAX),
        paired_at_ms: row.get(9)?,
    })
}

fn runtimes(conn: &Connection) -> rusqlite::Result<Vec<RuntimeRecord>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {RUNTIME_COLUMNS} FROM runtime ORDER BY position, paired_at_ms, runtime_id"
    ))?;
    stmt.query_map([], read_runtime)?.collect()
}

fn runtime(conn: &Connection, runtime_id: &str) -> Result<RuntimeRecord, SmeltError> {
    conn.query_row(
        &format!("SELECT {RUNTIME_COLUMNS} FROM runtime WHERE runtime_id = ?1"),
        params![runtime_id],
        read_runtime,
    )
    .optional()
    .map_err(db_error)?
    .ok_or_else(|| unknown_runtime(runtime_id))
}

fn ordered_ids(conn: &Connection) -> Result<Vec<String>, SmeltError> {
    Ok(runtimes(conn)
        .map_err(db_error)?
        .into_iter()
        .map(|record| record.runtime_id)
        .collect())
}

fn renumber(tx: &Transaction<'_>, ids: &[String]) -> Result<(), SmeltError> {
    for (position, id) in ids.iter().enumerate() {
        tx.execute(
            "UPDATE runtime SET position = ?2 WHERE runtime_id = ?1",
            params![id, position as i64],
        )
        .map_err(db_error)?;
    }
    Ok(())
}

fn delete_runtime_rows(tx: &Transaction<'_>, runtime_id: &str) -> Result<bool, SmeltError> {
    let deleted = tx
        .execute(
            "DELETE FROM runtime WHERE runtime_id = ?1",
            params![runtime_id],
        )
        .map_err(db_error)?;
    for table in ["draft", "draft_image"] {
        tx.execute(
            &format!("DELETE FROM {table} WHERE runtime_id = ?1"),
            params![runtime_id],
        )
        .map_err(db_error)?;
    }
    Ok(deleted > 0)
}

fn delete_draft_rows(
    tx: &Transaction<'_>,
    runtime_id: &str,
    session_id: &str,
) -> Result<(), SmeltError> {
    for table in ["draft", "draft_image"] {
        tx.execute(
            &format!("DELETE FROM {table} WHERE runtime_id = ?1 AND session_id = ?2"),
            params![runtime_id, session_id],
        )
        .map_err(db_error)?;
    }
    Ok(())
}

pub(crate) fn unknown_runtime(runtime_id: &str) -> SmeltError {
    error(
        SmeltErrorKind::Other,
        format!("没有登记这台机器：{runtime_id}"),
    )
}

fn db_error(failure: rusqlite::Error) -> SmeltError {
    error(SmeltErrorKind::Other, format!("本机数据库出错：{failure}"))
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
