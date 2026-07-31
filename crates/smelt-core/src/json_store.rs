//! 通用 JSON 配置文件读写：把 appearance/launch_config/llm_config/pet_config 各自手写一遍的
//! 「path → 读（缺失/损坏回退默认）→ 写（失败静默忽略）」样板收口成泛型函数。

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io::Write;
use std::path::PathBuf;

/// 读取 JSON 配置；文件缺失、内容损坏都回退默认值，不 panic 不报错。
pub fn load_json<T: DeserializeOwned + Default>(path: Option<PathBuf>) -> T {
    path.and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// 写回 JSON 配置（失败静默忽略：目录建不出来 / 序列化失败 / 写盘失败都不影响主流程）。
pub fn save_json<T: Serialize>(path: Option<PathBuf>, v: &T) {
    let Some(path) = path else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(v) {
        let _ = std::fs::write(path, json);
    }
}

/// 写回包含访问令牌等敏感信息的 JSON 配置。
///
/// Unix 上无论文件是首次创建还是已经存在，权限都会收紧为仅当前用户可读写。
pub fn save_json_private<T: Serialize>(path: Option<PathBuf>, v: &T) {
    let Some(path) = path else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(json) = serde_json::to_string_pretty(v) else {
        return;
    };

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let Ok(mut file) = options.open(&path) else {
        return;
    };

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
    }

    let _ = file.write_all(json.as_bytes());
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn private_json_is_saved_with_owner_only_permissions() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "smelt-private-json-{}-{nonce}.json",
            std::process::id()
        ));
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        save_json_private(Some(path.clone()), &serde_json::json!({"token": "secret"}));

        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&path).unwrap())
                .unwrap()["token"],
            "secret"
        );
        std::fs::remove_file(path).unwrap();
    }
}
