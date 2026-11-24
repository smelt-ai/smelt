//! 给每个插件的 SQLite 存储能力（封装 [`smelt_plugin_api::PluginStore`]）。
//!
//! 四个通用调用挂在 [`PluginHost`] 上：`store.query`、`store.execute`、`store.write`、
//! `store.migrate`。插件 id 由 Rust 闭包固定，插件无法越界读写其它插件的库。
//!
//! 每个插件维护自己的 FIFO 队列，仅在非空时投递到共享阻塞池（`smelt-io`）按顺序排空：
//! 同插件严格串行、跨插件零队头阻塞，空闲时零常驻线程，不阻塞 GPUI 主线程或 NSH 引擎线程。

#[cfg(test)]
use std::path::{Path, PathBuf};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};

use serde_json::{Map, Number, Value, json};
use smelt_plugin_api::{
    self as api, PluginStore,
    rusqlite::{self, types::ValueRef},
};

use crate::plugins::PluginHost;

/// JS 数字能精确表示的最大整数，`Number.MAX_SAFE_INTEGER`。
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

enum StoreRoot {
    /// 生产环境：走 `smelt_plugin_api::plugin_store(owner)`（`~/.smelt/plugin-data/<id>/store.sqlite3`）。
    Global,
    /// 测试隔离目录：`<root>/<id>/store.sqlite3`，不碰用户主目录。
    #[cfg(test)]
    Custom(PathBuf),
}

enum Op {
    Query {
        sql: String,
        params: Vec<rusqlite::types::Value>,
    },
    Execute {
        sql: String,
        params: Vec<rusqlite::types::Value>,
    },
    Write {
        statements: Vec<(String, Vec<rusqlite::types::Value>)>,
    },
    Migrate {
        steps: Vec<String>,
    },
}

struct PluginRequest {
    op: Op,
    done: Box<dyn FnOnce(Result<Value, String>) + Send>,
}

struct QueueSlot {
    running: bool,
    pending: VecDeque<PluginRequest>,
}

/// 插件 SQLite 能力后端：按插件 id 维护 FIFO 队列，按需投递到共享阻塞池排空
/// （同插件严格 FIFO、跨插件零队头阻塞、空闲零常驻线程）。
#[derive(Clone)]
pub(crate) struct StoreHub {
    root: Arc<StoreRoot>,
    queues: Arc<Mutex<HashMap<String, QueueSlot>>>,
    #[cfg(test)]
    custom_stores: Arc<Mutex<HashMap<String, Arc<PluginStore>>>>,
}

impl StoreHub {
    pub(crate) fn global() -> Self {
        Self::new(StoreRoot::Global)
    }

    #[cfg(test)]
    pub(crate) fn custom(root: PathBuf) -> Self {
        Self::new(StoreRoot::Custom(root))
    }

    fn new(root: StoreRoot) -> Self {
        Self {
            root: Arc::new(root),
            queues: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(test)]
            custom_stores: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 把四个通用 `store.*` 能力登记到 [`PluginHost`] 上。
    pub(crate) fn serve(&self, plugins: &PluginHost) {
        self.register(plugins, "store.query", |data| {
            let (sql, params) = parse_sql_and_params("store.query", data)?;
            Ok(Op::Query { sql, params })
        });
        self.register(plugins, "store.execute", |data| {
            let (sql, params) = parse_sql_and_params("store.execute", data)?;
            Ok(Op::Execute { sql, params })
        });
        self.register(plugins, "store.write", |data| {
            let statements = parse_write_statements(data)?;
            Ok(Op::Write { statements })
        });
        self.register(plugins, "store.migrate", |data| {
            let steps = parse_string_list("store.migrate", data.get("steps"))?;
            Ok(Op::Migrate { steps })
        });
    }

    fn submit(
        &self,
        plugin: &str,
        op: Op,
        done: impl FnOnce(Result<Value, String>) + Send + 'static,
    ) {
        if plugin.is_empty() {
            done(Err("插件没有来源身份，不能用库".to_owned()));
            return;
        }
        let should_spawn = {
            let mut queues = self.queues.lock().unwrap_or_else(|e| e.into_inner());
            let slot = queues
                .entry(plugin.to_owned())
                .or_insert_with(|| QueueSlot {
                    running: false,
                    pending: VecDeque::new(),
                });
            slot.pending.push_back(PluginRequest {
                op,
                done: Box::new(done),
            });
            if slot.running {
                false
            } else {
                slot.running = true;
                true
            }
        };
        if should_spawn {
            let hub = self.clone();
            let owner = plugin.to_owned();
            api::blocking::spawn_detached(move || hub.drain_plugin(owner));
        }
    }

    fn drain_plugin(&self, owner: String) {
        struct DrainGuard<'a> {
            hub: &'a StoreHub,
            owner: &'a str,
            finished: bool,
        }

        impl Drop for DrainGuard<'_> {
            fn drop(&mut self) {
                if self.finished {
                    return;
                }
                let respawn = {
                    let mut queues = self.hub.queues.lock().unwrap_or_else(|e| e.into_inner());
                    match queues.get_mut(self.owner) {
                        Some(slot) if slot.pending.is_empty() => {
                            queues.remove(self.owner);
                            false
                        }
                        Some(slot) => {
                            slot.running = true;
                            true
                        }
                        None => false,
                    }
                };
                if respawn {
                    let hub = self.hub.clone();
                    let owner = self.owner.to_owned();
                    api::blocking::spawn_detached(move || hub.drain_plugin(owner));
                }
            }
        }

        let mut guard = DrainGuard {
            hub: self,
            owner: &owner,
            finished: false,
        };

        loop {
            let request = {
                let mut queues = self.queues.lock().unwrap_or_else(|e| e.into_inner());
                let Some(slot) = queues.get_mut(&owner) else {
                    guard.finished = true;
                    return;
                };
                if let Some(next) = slot.pending.pop_front() {
                    next
                } else {
                    queues.remove(&owner);
                    guard.finished = true;
                    return;
                }
            };
            let result = self
                .open_cached_store(&owner)
                .and_then(|store| execute_op(&store, request.op));
            (request.done)(result);
        }
    }

    fn open_cached_store(&self, owner: &str) -> Result<Arc<PluginStore>, String> {
        match self.root.as_ref() {
            StoreRoot::Global => open_store(&self.root, owner),
            #[cfg(test)]
            StoreRoot::Custom(_) => {
                {
                    let stores = self.custom_stores.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(store) = stores.get(owner) {
                        return Ok(Arc::clone(store));
                    }
                }
                let opened = open_store(&self.root, owner)?;
                let mut stores = self.custom_stores.lock().unwrap_or_else(|e| e.into_inner());
                Ok(Arc::clone(stores.entry(owner.to_owned()).or_insert(opened)))
            }
        }
    }

    fn register(
        &self,
        plugins: &PluginHost,
        method: &'static str,
        parse: impl Fn(&Value) -> Result<Op, String> + Send + Sync + 'static,
    ) {
        let hub = self.clone();
        plugins.handle(method, move |plugin, call| {
            if plugin == crate::plugins::BUILTINS_VM_ID {
                return call.reject("共享内置插件调用存储缺少成员插件身份");
            }
            let op = match parse(call.data()) {
                Ok(op) => op,
                Err(error) => return call.reject(error),
            };
            hub.submit(plugin, op, move |result| call.reply(result));
        });
    }
}

fn open_store(root: &StoreRoot, owner: &str) -> Result<Arc<PluginStore>, String> {
    if owner.is_empty() {
        return Err("插件没有来源身份，不能用库".to_owned());
    }
    match root {
        StoreRoot::Global => api::plugin_store(owner).map_err(|error| error.to_string()),
        #[cfg(test)]
        StoreRoot::Custom(base) => {
            validate_owner(owner)?;
            let path = custom_store_path(base, owner);
            PluginStore::open(path)
                .map(Arc::new)
                .map_err(|error| error.to_string())
        }
    }
}

#[cfg(test)]
fn custom_store_path(base: &Path, owner: &str) -> PathBuf {
    base.join(owner).join("store.sqlite3")
}

#[cfg(test)]
fn validate_owner(owner: &str) -> Result<(), String> {
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
    if owner.is_empty() || owner.starts_with('.') || !owner.chars().all(allowed) {
        return Err(format!(
            "插件来源 `{owner}` 不能作为数据目录名：仅允许字母、数字和 . _ -，且不能以 . 开头"
        ));
    }
    Ok(())
}

fn execute_op(store: &PluginStore, op: Op) -> Result<Value, String> {
    match op {
        Op::Query { sql, params } => store_query(store, &sql, params),
        Op::Execute { sql, params } => store_execute(store, &sql, params),
        Op::Write { statements } => store_write(store, statements),
        Op::Migrate { steps } => {
            let refs: Vec<&str> = steps.iter().map(String::as_str).collect();
            store
                .migrate(&refs)
                .map(|()| Value::Null)
                .map_err(|error| format!("store.migrate：{error}"))
        }
    }
}

fn store_query(
    store: &PluginStore,
    sql: &str,
    params: Vec<rusqlite::types::Value>,
) -> Result<Value, String> {
    let rows = store
        .read(|db| {
            let mut statement = db.prepare(sql)?;
            let columns: Vec<String> = statement.column_names().map(str::to_owned).collect();
            let mut rows = statement.query(rusqlite::params_from_iter(params))?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                let mut object = Map::with_capacity(columns.len());
                for (index, column) in columns.iter().enumerate() {
                    let value = match row.get_ref(index)? {
                        ValueRef::Null => Value::Null,
                        ValueRef::Integer(value) => Value::Number(value.into()),
                        ValueRef::Real(value) => {
                            Number::from_f64(value).map_or(Value::Null, Value::Number)
                        }
                        ValueRef::Text(text) => {
                            Value::String(String::from_utf8_lossy(text).into_owned())
                        }
                        ValueRef::Blob(_) => {
                            return Err(rusqlite::Error::InvalidColumnType(
                                index,
                                column.clone(),
                                rusqlite::types::Type::Blob,
                            ));
                        }
                    };
                    object.insert(column.clone(), value);
                }
                out.push(Value::Object(object));
            }
            Ok(out)
        })
        .map_err(|error| format!("store.query：{error}"))?;
    Ok(Value::Array(rows))
}

fn store_execute(
    store: &PluginStore,
    sql: &str,
    params: Vec<rusqlite::types::Value>,
) -> Result<Value, String> {
    let changed = store
        .read(|db| db.execute(sql, rusqlite::params_from_iter(params)))
        .map_err(|error| format!("store.execute：{error}"))?;
    Ok(json!(changed))
}

fn store_write(
    store: &PluginStore,
    statements: Vec<(String, Vec<rusqlite::types::Value>)>,
) -> Result<Value, String> {
    let changed = store
        .write(|tx| {
            statements
                .iter()
                .map(|(sql, params)| tx.execute(sql, rusqlite::params_from_iter(params.iter())))
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(|error| format!("store.write：{error}"))?;
    Ok(json!(changed))
}

fn parse_sql_and_params(
    method: &str,
    value: &Value,
) -> Result<(String, Vec<rusqlite::types::Value>), String> {
    let Some(sql) = value.get("sql").and_then(Value::as_str) else {
        return Err(format!("{method}：缺 sql 字符串"));
    };
    let params = parse_params(method, value.get("params"))?;
    Ok((sql.to_owned(), params))
}

fn parse_params(
    method: &str,
    value: Option<&Value>,
) -> Result<Vec<rusqlite::types::Value>, String> {
    use rusqlite::types::Value as Sql;
    match value {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::Null => Ok(Sql::Null),
                Value::Bool(flag) => Ok(Sql::Integer(i64::from(*flag))),
                Value::Number(number) => {
                    if let Some(integer) = number.as_i64() {
                        Ok(Sql::Integer(integer))
                    } else if let Some(unsigned) = number.as_u64()
                        && let Ok(integer) = i64::try_from(unsigned)
                    {
                        Ok(Sql::Integer(integer))
                    } else if let Some(float) = number.as_f64() {
                        if float.fract() == 0.0 && float.abs() <= MAX_SAFE_INTEGER {
                            Ok(Sql::Integer(float as i64))
                        } else {
                            Ok(Sql::Real(float))
                        }
                    } else {
                        Err(format!("{method}：不支持的数字参数 {number}"))
                    }
                }
                Value::String(text) => Ok(Sql::Text(text.clone())),
                other => Err(format!(
                    "{method}：参数只能是字符串、数字、布尔或 null，收到 {other}"
                )),
            })
            .collect(),
        Some(other) => Err(format!("{method}：params 要是数组，收到 {other}")),
    }
}

fn parse_write_statements(
    value: &Value,
) -> Result<Vec<(String, Vec<rusqlite::types::Value>)>, String> {
    let Some(items) = value.get("statements").and_then(Value::as_array) else {
        return Err("store.write：缺 statements 数组 [{ sql, params? }]".to_owned());
    };
    items
        .iter()
        .map(|item| parse_sql_and_params("store.write", item))
        .collect()
}

fn parse_string_list(method: &str, value: Option<&Value>) -> Result<Vec<String>, String> {
    let Some(items) = value.and_then(Value::as_array) else {
        return Err(format!("{method}：要是字符串数组"));
    };
    items
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{method}：数组元素要是字符串，收到 {item}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    fn temp_root() -> PathBuf {
        std::env::temp_dir().join(format!("smelt-nsh-store-test-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn migrate_write_query_and_plugin_isolation() {
        let root = temp_root();
        let store_a = open_store(&StoreRoot::Custom(root.clone()), "alpha").unwrap();
        let store_b = open_store(&StoreRoot::Custom(root.clone()), "beta").unwrap();

        execute_op(
            &store_a,
            Op::Migrate {
                steps: vec!["CREATE TABLE pin (id TEXT PRIMARY KEY, pos INTEGER NOT NULL);".into()],
            },
        )
        .unwrap();
        execute_op(
            &store_a,
            Op::Write {
                statements: vec![
                    (
                        "INSERT INTO pin (id, pos) VALUES (?1, ?2)".into(),
                        vec![
                            rusqlite::types::Value::Text("c1".into()),
                            rusqlite::types::Value::Integer(0),
                        ],
                    ),
                    (
                        "INSERT INTO pin (id, pos) VALUES (?1, ?2)".into(),
                        vec![
                            rusqlite::types::Value::Text("c2".into()),
                            rusqlite::types::Value::Integer(1),
                        ],
                    ),
                ],
            },
        )
        .unwrap();

        let rows = execute_op(
            &store_a,
            Op::Query {
                sql: "SELECT id, pos FROM pin ORDER BY pos".into(),
                params: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(
            rows,
            json!([{ "id": "c1", "pos": 0 }, { "id": "c2", "pos": 1 }])
        );

        // 事务里失败整批回滚。
        let failed = execute_op(
            &store_a,
            Op::Write {
                statements: vec![
                    (
                        "INSERT INTO pin (id, pos) VALUES ('c3', 2)".into(),
                        Vec::new(),
                    ),
                    ("INSERT INTO missing_table VALUES (1)".into(), Vec::new()),
                ],
            },
        );
        assert!(failed.is_err());
        let after = execute_op(
            &store_a,
            Op::Query {
                sql: "SELECT count(*) AS n FROM pin".into(),
                params: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(after, json!([{ "n": 2 }]));

        // beta 独立的 SQLite 文件里看不到 alpha 的表。
        let isolated = execute_op(
            &store_b,
            Op::Query {
                sql: "SELECT * FROM pin".into(),
                params: Vec::new(),
            },
        );
        assert!(isolated.is_err());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn per_plugin_workers_do_not_block_each_other_and_preserve_fifo() {
        let root = temp_root();
        let hub = StoreHub::custom(root.clone());

        let submit = |plugin: &str, op: Op| -> mpsc::Receiver<Result<Value, String>> {
            let (tx, rx) = mpsc::channel();
            hub.submit(plugin, op, move |result| {
                let _ = tx.send(result);
            });
            rx
        };

        // 同插件按 FIFO 执行：migrate -> write -> query 依次成功。
        submit(
            "alpha",
            Op::Migrate {
                steps: vec!["CREATE TABLE kv (k TEXT PRIMARY KEY);".into()],
            },
        )
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap()
        .unwrap();

        submit(
            "beta",
            Op::Migrate {
                steps: vec!["CREATE TABLE items (id INTEGER PRIMARY KEY);".into()],
            },
        )
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap()
        .unwrap();

        // 在外部连接上持住 alpha 的 EXCLUSIVE 锁，让 alpha worker 的写事务阻塞在 SQLite 锁里。
        let blocker = rusqlite::Connection::open(custom_store_path(&root, "alpha")).unwrap();
        blocker.execute_batch("BEGIN EXCLUSIVE;").unwrap();

        let alpha_rx = submit(
            "alpha",
            Op::Write {
                statements: vec![("INSERT INTO kv (k) VALUES ('k1')".into(), Vec::new())],
            },
        );
        let alpha_rx2 = submit(
            "alpha",
            Op::Write {
                statements: vec![("INSERT INTO kv (k) VALUES ('k2')".into(), Vec::new())],
            },
        );

        // alpha 被外部锁阻塞期间，beta 自己的队列应立即完成查询，不受 alpha 队头阻塞。
        let beta_rows = submit(
            "beta",
            Op::Query {
                sql: "SELECT count(*) AS n FROM items".into(),
                params: Vec::new(),
            },
        )
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap()
        .unwrap();
        assert_eq!(beta_rows, json!([{ "n": 0 }]));
        assert!(
            alpha_rx.try_recv().is_err(),
            "alpha first write should still be waiting on lock"
        );
        assert!(
            alpha_rx2.try_recv().is_err(),
            "alpha second write should stay queued behind first write"
        );

        // 释放 alpha 的外部锁，alpha 两条写入按严格 FIFO 完成，后续读看得到顺序结果。
        blocker.execute_batch("COMMIT;").unwrap();
        drop(blocker);
        alpha_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
            .unwrap();
        alpha_rx2
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
            .unwrap();
        let alpha_rows = submit(
            "alpha",
            Op::Query {
                sql: "SELECT k FROM kv ORDER BY rowid".into(),
                params: Vec::new(),
            },
        )
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap()
        .unwrap();
        assert_eq!(alpha_rows, json!([{ "k": "k1" }, { "k": "k2" }]));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            if hub
                .queues
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty()
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "queues slot should be cleaned up after draining"
            );
            std::thread::yield_now();
        }

        drop(hub);
        let _ = std::fs::remove_dir_all(root);
    }
}
