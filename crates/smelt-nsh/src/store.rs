//! 给每个独立插件 VM 的 SQLite 存储能力（封装 [`smelt_plugin_api::PluginStore`]）。
//!
//! 四个通用调用挂在 [`PluginHost`] 上：`store.query`、`store.execute`、`store.write`、
//! `store.migrate`。插件 id 由 Rust 闭包固定，插件无法越界读写其它插件的库。
//!
//! 所有磁盘 IO 都在独立后台线程串行执行（平时睡在 channel 上，零轮询，发端全放掉即退出），
//! 不阻塞 GPUI 主线程，也不阻塞 NSH 引擎线程。

#[cfg(test)]
use std::path::{Path, PathBuf};
use std::{
    collections::HashMap,
    sync::{Arc, mpsc},
};

use nsh_engine::HostCall;
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

struct Request {
    plugin: String,
    op: Op,
    call: HostCall,
}

/// 插件 SQLite 能力后端：持有后台工作线程发端，随 [`PluginHost`] 释放自动停机。
#[derive(Clone)]
pub(crate) struct StoreHub {
    sender: mpsc::Sender<Request>,
}

impl StoreHub {
    pub(crate) fn global() -> Self {
        Self::start(StoreRoot::Global)
    }

    #[cfg(test)]
    pub(crate) fn in_dir(root: PathBuf) -> Self {
        Self::start(StoreRoot::Custom(root))
    }

    fn start(root: StoreRoot) -> Self {
        let (sender, receiver) = mpsc::channel::<Request>();
        std::thread::Builder::new()
            .name("smelt-nsh-plugin-store".into())
            .spawn(move || worker_loop(root, receiver))
            .expect("spawn smelt-nsh plugin store worker");
        Self { sender }
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

    fn register(
        &self,
        plugins: &PluginHost,
        method: &'static str,
        parse: impl Fn(&Value) -> Result<Op, String> + Send + Sync + 'static,
    ) {
        let sender = self.sender.clone();
        plugins.handle(method, move |plugin, call| {
            let op = match parse(call.data()) {
                Ok(op) => op,
                Err(error) => return call.reject(error),
            };
            if let Err(unsent) = sender.send(Request {
                plugin: plugin.to_owned(),
                op,
                call,
            }) {
                unsent.0.call.reject("插件存储服务已关闭");
            }
        });
    }
}

fn worker_loop(root: StoreRoot, receiver: mpsc::Receiver<Request>) {
    let mut custom_stores: HashMap<String, Arc<PluginStore>> = HashMap::new();
    while let Ok(request) = receiver.recv() {
        let result = open_store(&root, &mut custom_stores, &request.plugin)
            .and_then(|store| execute_op(&store, request.op));
        request.call.reply(result);
    }
}

fn open_store(
    root: &StoreRoot,
    _custom_stores: &mut HashMap<String, Arc<PluginStore>>,
    owner: &str,
) -> Result<Arc<PluginStore>, String> {
    if owner.is_empty() {
        return Err("插件没有来源身份，不能用库".to_owned());
    }
    match root {
        StoreRoot::Global => api::plugin_store(owner).map_err(|error| error.to_string()),
        #[cfg(test)]
        StoreRoot::Custom(base) => {
            validate_owner(owner)?;
            if let Some(store) = _custom_stores.get(owner) {
                return Ok(store.clone());
            }
            let path = custom_store_path(base, owner);
            let store = Arc::new(PluginStore::open(path).map_err(|error| error.to_string())?);
            _custom_stores.insert(owner.to_owned(), store.clone());
            Ok(store)
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
    use super::*;

    fn temp_root() -> PathBuf {
        std::env::temp_dir().join(format!("smelt-nsh-store-test-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn migrate_write_query_and_plugin_isolation() {
        let root = temp_root();
        let mut stores = HashMap::new();
        let store_a = open_store(&StoreRoot::Custom(root.clone()), &mut stores, "alpha").unwrap();
        let store_b = open_store(&StoreRoot::Custom(root.clone()), &mut stores, "beta").unwrap();

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
}
