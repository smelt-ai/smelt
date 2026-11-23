//! App 里各插件加载结果的快照，给 `smelt plugin status` 读（见 `cli`）。
//!
//! 权威是 App 内存里的注册表；这里只是它的只读副本，每次挂载、重挂完整份重写一次，App
//! 退出时删掉。快照记着写它的进程，读的一方据此分清「App 开着」和「上次没删掉的旧快照」。
//! 只放 `~/.smelt/run/`，不放进插件目录：插件目录有文件变就重挂（见 `watch`），写在那里
//! 会自己触发自己。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const FILE_NAME: &str = "plugin-status.json";

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Snapshot {
    /// 写快照的 App 进程。
    pub(super) pid: u32,
    pub(super) plugins: Vec<Entry>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Entry {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) version: String,
    pub(super) path: PathBuf,
    /// 加载失败的原因；加载好了是 `None`。
    pub(super) error: Option<String>,
}

fn path() -> Option<PathBuf> {
    smelt_paths::smelt_home().map(|home| home.join("run").join(FILE_NAME))
}

/// 挂载完写一份。写不了只记日志：快照只给命令行看，不影响插件本身。
pub(crate) fn publish(plugins: &[smelt_nsh::LoadedPluginInfo]) {
    let snapshot = Snapshot {
        pid: std::process::id(),
        plugins: plugins
            .iter()
            .map(|plugin| Entry {
                id: plugin.id.clone(),
                name: plugin.name.clone(),
                version: plugin.version.clone(),
                path: plugin.root.clone(),
                error: plugin.error.clone(),
            })
            .collect(),
    };
    let Some(path) = path() else {
        return;
    };
    if let Err(error) = write(&path, &snapshot) {
        eprintln!("[plugins] 写 {} 失败：{error}", path.display());
    }
}

/// App 退出时删掉自己写的快照。
pub(super) fn retract() {
    let Some(path) = path() else {
        return;
    };
    if read_at(&path).is_some_and(|snapshot| snapshot.pid == std::process::id()) {
        let _ = std::fs::remove_file(path);
    }
}

/// 开着的 App 写的快照。没有快照、写它的进程已经不在了，都算 App 没开。
pub(super) fn read_live() -> Option<Snapshot> {
    read_at(&path()?).filter(|snapshot| smelt_sys::process::pid_is_alive(snapshot.pid))
}

fn read_at(path: &std::path::Path) -> Option<Snapshot> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// 先写临时文件再改名，读的一方不会读到半截。
fn write(path: &std::path::Path, snapshot: &Snapshot) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let staging = path.with_extension(format!("json.{}", std::process::id()));
    std::fs::write(&staging, serde_json::to_vec_pretty(snapshot)?)?;
    std::fs::rename(&staging, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&staging);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_round_trips_through_its_file() {
        let dir =
            std::env::temp_dir().join(format!("smelt-plugin-status-{}", uuid::Uuid::new_v4()));
        let path = dir.join("run").join(FILE_NAME);
        let snapshot = Snapshot {
            pid: std::process::id(),
            plugins: vec![Entry {
                id: "com.example.hello".into(),
                name: "你好".into(),
                version: "0.1.0".into(),
                path: "/tmp/hello".into(),
                error: Some("SyntaxError".into()),
            }],
        };
        write(&path, &snapshot).unwrap();
        assert_eq!(read_at(&path), Some(snapshot));
        // 只剩快照本身，临时文件已经改名走了。
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
