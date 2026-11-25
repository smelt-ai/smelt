//! App 里各插件加载结果的快照，给 `smelt plugin status` 读（见 `cli`）。
//!
//! 权威是 App 内存里的注册表；这里只是它的只读副本，每次挂载、重挂完整份重写一次，App
//! 退出时删掉。快照记着写它的进程，读的一方据此分清「App 开着」和「上次没删掉的旧快照」。
//! 只放 `~/.smelt/run/`，不放进插件目录：插件目录有文件变就重挂（见 `watch`），写在那里
//! 会自己触发自己。

use std::{path::PathBuf, sync::Mutex};

use serde::{Deserialize, Serialize};
use smelt_nsh::PluginSource;

const FILE_NAME: &str = "plugin-status.json";

static LAST_PUBLISHED: Mutex<Option<Snapshot>> = Mutex::new(None);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Snapshot {
    /// 写快照的 App 进程。
    pub(super) pid: u32,
    pub(super) plugins: Vec<Entry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Entry {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) version: String,
    pub(super) source: String,
    #[serde(default)]
    pub(super) overrides_builtin: bool,
    #[serde(default)]
    pub(super) fallback_to_builtin: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) module: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) uses_modules: Vec<String>,
    pub(super) path: PathBuf,
    #[serde(default)]
    pub(super) views: Vec<String>,
    /// 加载失败的原因；加载好了是 `None`。
    pub(super) error: Option<String>,
    /// 用户插件这次加载时源码的指纹；`cli` 现算一份比对，看是不是最新源码的结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) source_fingerprint: Option<String>,
}

impl Entry {
    /// 用户插件的源码在加载之后又改过（App 还没按最新的重新加载完，或者刚保存还没轮到）。
    pub(super) fn is_stale(&self) -> bool {
        self.source_fingerprint.as_ref().is_some_and(|loaded| {
            smelt_nsh::source_fingerprint(&self.path).as_ref() != Some(loaded)
        })
    }
}

fn source_label(source: PluginSource) -> String {
    match source {
        PluginSource::Builtin => "builtin",
        PluginSource::User => "user",
    }
    .to_owned()
}

fn path() -> Option<PathBuf> {
    smelt_paths::smelt_home().map(|home| home.join("run").join(FILE_NAME))
}

/// 挂载完写一份（和上次快照完全一致时跳过写盘）。写不了只记日志：快照只给命令行看，不影响插件本身。
pub(crate) fn publish(plugins: &[smelt_nsh::LoadedPluginInfo]) {
    let snapshot = Snapshot {
        pid: std::process::id(),
        plugins: plugins
            .iter()
            .map(|plugin| Entry {
                id: plugin.id.clone(),
                name: plugin.name.clone(),
                version: plugin.version.clone(),
                source: source_label(plugin.source),
                overrides_builtin: plugin.overrides_builtin,
                fallback_to_builtin: plugin.fallback_to_builtin,
                module: plugin.module.clone(),
                uses_modules: plugin.used_modules.clone(),
                path: plugin.root.clone(),
                views: plugin.views.clone(),
                error: plugin.error.clone(),
                source_fingerprint: plugin.source_fingerprint.clone(),
            })
            .collect(),
    };
    {
        let last = LAST_PUBLISHED.lock().unwrap_or_else(|e| e.into_inner());
        if last.as_ref() == Some(&snapshot) {
            return;
        }
    }
    let Some(path) = path() else {
        return;
    };
    match write(&path, &snapshot) {
        Ok(()) => {
            *LAST_PUBLISHED.lock().unwrap_or_else(|e| e.into_inner()) = Some(snapshot);
        }
        Err(error) => eprintln!("[plugins] 写 {} 失败：{error}", path.display()),
    }
}

/// App 退出时删掉自己写的快照。
pub(super) fn retract() {
    *LAST_PUBLISHED.lock().unwrap_or_else(|e| e.into_inner()) = None;
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
                source: "user".into(),
                overrides_builtin: false,
                fallback_to_builtin: false,
                module: Some("com/example".into()),
                uses_modules: vec!["smelt.conversation".into()],
                path: "/tmp/hello".into(),
                views: vec!["com.example.hello/main".into()],
                error: Some("SyntaxError".into()),
                source_fingerprint: Some("0123456789abcdef".into()),
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

    #[test]
    fn an_entry_is_stale_once_its_source_changes() {
        let dir = std::env::temp_dir().join(format!("smelt-plugin-stale-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("nsh.tsx"), "export const a = 1;").unwrap();
        let mut entry = Entry {
            id: "com.example.hello".into(),
            name: "Hello".into(),
            version: "0.1.0".into(),
            source: "user".into(),
            overrides_builtin: false,
            fallback_to_builtin: false,
            module: None,
            uses_modules: Vec::new(),
            path: dir.clone(),
            views: Vec::new(),
            error: None,
            source_fingerprint: smelt_nsh::source_fingerprint(&dir),
        };
        assert!(!entry.is_stale());
        // 只改了测试和隐藏目录，不算：加载时也不读它们。
        std::fs::create_dir_all(dir.join("tests")).unwrap();
        std::fs::write(dir.join("tests/a.test.ts"), "x").unwrap();
        assert!(!entry.is_stale());
        std::fs::write(dir.join("nsh.tsx"), "export const a = 2;").unwrap();
        assert!(entry.is_stale());
        // 自带插件不比。
        entry.source_fingerprint = None;
        assert!(!entry.is_stale());
        let _ = std::fs::remove_dir_all(dir);
    }
}
