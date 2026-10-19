//! 运行时身份：第一次启动时生成一个 UUID 存进运行时库，之后永久不变。
//!
//! 客户端按它区分运行时，和怎么连上无关（socket 路径、iroh endpoint id 都可能变）。
//! 只有 smeltd 写这一行；GUI 不碰。

use std::sync::{Arc, Mutex};

/// 只缓存成功的结果。库暂时打不开时 describe 回 `None`，下一次再试，绝不发一个
/// 没落盘的临时 id：那样运行时一重启，客户端就会把它当成另一台机器。
static RUNTIME_ID: Mutex<Option<Arc<str>>> = Mutex::new(None);

pub(crate) fn runtime_id() -> Option<Arc<str>> {
    // 锁住整个读-生成-写，保证同一进程里并发的 describe 拿到同一个 id。
    let mut cached = RUNTIME_ID.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(id) = cached.as_ref() {
        return Some(Arc::clone(id));
    }
    // 测试进程默认不开 SQLite 路由，也就没有运行时库可读；这不是故障，不记日志。
    if !smelt_core::sqlite_state::is_sqlite_state_enabled() {
        return None;
    }
    let loaded =
        smelt_core::sqlite_state::default_sqlite_store().and_then(|store| load_or_create(&store));
    match loaded {
        Ok(id) => {
            let id: Arc<str> = id.into();
            *cached = Some(Arc::clone(&id));
            Some(id)
        }
        Err(error) => {
            smelt_core::app_log::error("smeltd", &format!("读取运行时身份失败：{error}"));
            None
        }
    }
}

fn load_or_create(store: &smelt_store::Store) -> Result<String, String> {
    if let Some(id) = store.runtime_id()? {
        if uuid::Uuid::parse_str(&id).is_err() {
            return Err(format!("运行时身份不是 UUID：{id:?}"));
        }
        return Ok(id);
    }
    let id = uuid::Uuid::new_v4().to_string();
    store.put_runtime_id(&id)?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (smelt_store::Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "smeltd-runtime-id-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store =
            smelt_core::sqlite_state::open_sqlite_store(&dir.join(smelt_store::DATABASE_FILE_NAME))
                .unwrap();
        (store, dir)
    }

    #[test]
    fn runtime_id_is_created_once_and_then_stable() {
        let (store, dir) = temp_store();

        let first = load_or_create(&store).unwrap();
        let second = load_or_create(&store).unwrap();

        assert!(uuid::Uuid::parse_str(&first).is_ok());
        assert_eq!(first, second);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 损坏的值不能被悄悄换成新 id：换了就等于换了一台机器。
    #[test]
    fn corrupt_runtime_id_is_an_error_not_a_new_identity() {
        let (store, dir) = temp_store();
        store.put_runtime_id("not-a-uuid").unwrap();

        assert!(load_or_create(&store).is_err());
        assert_eq!(store.runtime_id().unwrap().as_deref(), Some("not-a-uuid"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
