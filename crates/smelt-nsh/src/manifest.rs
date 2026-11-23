//! 主壳的插件发现、清单读取与多目录热重载（`smelt-plugin.json`）。
//!
//! 扫描两处目录：
//! 1. App 自带插件目录（开发态读仓库 `plugins/`，发版 `.app` 读 `Contents/Resources/plugins/`）；
//! 2. 用户插件目录（`~/.smelt/plugins/`）。
//!
//! 规则与旧插件系统一致：同 id 先发现者胜出（自带优先），重复项记为错误不加载；
//! 插件可附带 `themes/*.json`（或 JSON entry 的纯主题包），统一写入主题目录；
//! 监听目录（开发态含自带目录，生产态仅用户目录）文件变化后防抖 200ms 仅重编改动的插件。

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
    sync::mpsc::{self, RecvTimeoutError},
    time::Duration,
};

use notify::Watcher as _;
use nsh_core::element::description::SetDescription;
use tokio::sync::mpsc::UnboundedSender;

use crate::plugins::{Change, Plugins};

/// NSH 插件清单文件名。
pub const MANIFEST_FILE: &str = "smelt-plugin.json";

/// 防抖窗口：一次保存常伴随多条 FSEvents。
const QUIET: Duration = Duration::from_millis(200);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginSource {
    Builtin,
    User,
}

#[derive(Clone, Debug)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub entry: String,
}

impl PluginManifest {
    pub fn read(root: &Path) -> Result<Self, String> {
        let path = root.join(MANIFEST_FILE);
        let raw = fs::read_to_string(&path)
            .map_err(|error| format!("读取 {} 失败：{error}", path.display()))?;
        let value: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|error| format!("解析 {} 失败：{error}", path.display()))?;
        let obj = value
            .as_object()
            .ok_or_else(|| format!("{} 顶层必须是 JSON 对象", path.display()))?;
        let id = obj
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let name = obj
            .get("name")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| format!("{} 缺少 name", path.display()))?
            .to_owned();
        let version = obj
            .get("version")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("0.0.0")
            .to_owned();
        let entry = obj
            .get("entry")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let manifest = Self {
            id,
            name,
            version,
            entry,
        };
        if manifest.id.trim().is_empty() || manifest.id.contains('/') {
            return Err(format!(
                "{} 的 id 不合法：{:?}",
                path.display(),
                manifest.id
            ));
        }
        if manifest.entry.trim().is_empty() {
            return Err(format!("{} 缺少 entry", path.display()));
        }
        Ok(manifest)
    }

    pub fn is_theme_only(&self) -> bool {
        Path::new(&self.entry)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
    }
}

/// 一份插件目录配置（由应用组装层传入）。
#[derive(Clone, Debug)]
pub struct SourceDir {
    pub path: PathBuf,
    pub source: PluginSource,
    /// 是否监听热重载（开发态自带目录和用户目录为 true，发版包自带目录只读为 false）。
    pub watch: bool,
}

/// 检查插件目录是否合法（清单、文案、主题、以及非纯主题包的 TSX 打包与 import 校验）。
pub fn check_plugin(root: &Path, ui_sets: Vec<SetDescription>) -> Result<PluginManifest, String> {
    let manifest = PluginManifest::read(root)?;
    let entry_path = Path::new(&manifest.entry);
    if entry_path.is_absolute()
        || entry_path.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(format!(
            "插件入口必须是插件目录内的相对路径：{}",
            manifest.entry
        ));
    }
    if !root.join(entry_path).is_file() {
        return Err(format!("插件入口 {} 不存在", manifest.entry));
    }
    read_texts(root)?;
    read_themes(root, &manifest.entry)?;
    if manifest.is_theme_only() {
        return Ok(manifest);
    }
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let stage = std::env::temp_dir().join(format!("smelt-nsh-check-{}", uuid::Uuid::new_v4()));
    let _cleanup = Cleanup(stage.clone());
    let mut plugins = Plugins::new(&stage, "export function Shell() { return null; }", ui_sets)?;
    if let Change::Install(compiled) = plugins.install_entry(&manifest.id, root, &manifest.entry)? {
        crate::plugins::validate_unresolved_references(&manifest.id, compiled.code())?;
    }
    Ok(manifest)
}

/// 暴露给设置「面板插件」页的插件状态快照。
#[derive(Clone, Debug)]
pub struct LoadedPluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub root: PathBuf,
    pub source: PluginSource,
    pub error: Option<String>,
}

/// 一轮发现/热重载推回主线程的完整批次。
pub(crate) struct ShellUpdate {
    pub changes: Vec<Change>,
    pub removed_ids: Vec<String>,
    pub loaded: Vec<LoadedPluginInfo>,
    pub themes: Vec<(String, Vec<String>)>,
    /// 本轮编译成功的插件文案（插件 id → 语言表），由主线程在清旧登记后、启新 VM 前写入宿主桥。
    pub texts: BTreeMap<String, serde_json::Value>,
    /// 首次扫描启动的脚本插件 id 列表（非首次热重载为 `None`），供主壳等待首批初始化就绪。
    pub initial_scripts: Option<Vec<String>>,
}

struct Discovered {
    root: PathBuf,
    manifest: PluginManifest,
    source: PluginSource,
    duplicate_of: Option<PathBuf>,
}

fn discover(sources: &[SourceDir]) -> Vec<Discovered> {
    let mut found: Vec<Discovered> = Vec::new();
    for src in sources {
        let Ok(entries) = fs::read_dir(&src.path) else {
            continue;
        };
        let mut roots: Vec<PathBuf> = entries
            .flatten()
            .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
            .map(|entry| entry.path())
            .filter(|path| path.join(MANIFEST_FILE).is_file())
            .collect();
        roots.sort();
        for root in roots {
            match PluginManifest::read(&root) {
                Ok(manifest) => {
                    let duplicate_of = found
                        .iter()
                        .find(|seen| seen.duplicate_of.is_none() && seen.manifest.id == manifest.id)
                        .map(|seen| seen.root.clone());
                    found.push(Discovered {
                        root,
                        manifest,
                        source: src.source,
                        duplicate_of,
                    });
                }
                Err(error) => eprintln!("[smelt-nsh] {error}"),
            }
        }
    }
    found
}

fn read_texts(dir: &Path) -> Result<serde_json::Value, String> {
    let locales = dir.join("locales");
    let texts: BTreeMap<String, BTreeMap<String, String>> = if locales.is_dir() {
        rust_i18n_support::try_load_locales(&locales.to_string_lossy(), |_| false, true)?
    } else {
        BTreeMap::new()
    };
    Ok(serde_json::json!(texts))
}

fn read_themes(root: &Path, entry: &str) -> Result<Vec<String>, String> {
    let dir = root.join("themes");
    let mut files = match fs::read_dir(&dir) {
        Ok(entries) => entries
            .map(|e| e.map(|e| e.path()).map_err(|err| err.to_string()))
            .collect::<Result<Vec<_>, _>>()?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(format!("读取主题目录失败：{error}")),
    };
    files.retain(|p| p.extension().is_some_and(|ext| ext == "json"));
    let entry_path = Path::new(entry);
    if entry_path
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err("插件入口必须是插件目录内的相对路径".into());
    }
    if entry_path.extension().is_some_and(|ext| ext == "json") {
        let full = root.join(entry_path);
        if !files.contains(&full) {
            files.push(full);
        }
    }
    files.sort();
    files
        .into_iter()
        .map(|path| {
            fs::read_to_string(&path)
                .map_err(|error| format!("读取主题 {} 失败：{error}", path.display()))
        })
        .collect()
}

/// 后台发现与热重载句柄：释放后自动停止文件监听与后台线程。
pub(crate) struct ShellLive {
    _inputs: mpsc::Sender<Option<PathBuf>>,
    _watcher: Option<notify::RecommendedWatcher>,
}

impl ShellLive {
    pub(crate) fn start(
        sources: Vec<SourceDir>,
        stage: PathBuf,
        shell: &'static str,
        ui_sets: Vec<SetDescription>,
        updates: UnboundedSender<ShellUpdate>,
    ) -> Self {
        let (inputs, received) = mpsc::channel();
        let watcher = watch_sources(&sources, inputs.clone())
            .inspect_err(|error| eprintln!("[smelt-nsh] 监听插件目录失败：{error}"))
            .ok();
        std::thread::Builder::new()
            .name("smelt-nsh-shell-live".into())
            .spawn(move || {
                match Plugins::new(&stage, shell, ui_sets) {
                    Ok(plugins) => Worker {
                        sources,
                        plugins,
                        updates,
                        compiled_roots: BTreeMap::new(),
                    }
                    .run(&received),
                    Err(error) => eprintln!("[smelt-nsh] 主壳打包失败：{error}"),
                }
                if let Err(error) = fs::remove_dir_all(&stage) {
                    eprintln!("[smelt-nsh] 清理暂存目录 {} 失败：{error}", stage.display());
                }
            })
            .expect("spawn smelt-nsh shell live worker");
        Self {
            _inputs: inputs,
            _watcher: watcher,
        }
    }
}

struct Worker {
    sources: Vec<SourceDir>,
    plugins: Plugins,
    updates: UnboundedSender<ShellUpdate>,
    /// 插件 id → 上次成功打包的目录路径（用于按目录增量重编）。
    compiled_roots: BTreeMap<String, PathBuf>,
}

impl Worker {
    fn run(&mut self, inputs: &mpsc::Receiver<Option<PathBuf>>) {
        let initial = self.scan(None, true);
        if self.updates.send(initial).is_err() {
            return;
        }
        while let Ok(first) = inputs.recv() {
            let mut changed_roots = BTreeSet::new();
            let mut full_rescan = first.is_none();
            if let Some(root) = first {
                changed_roots.insert(root);
            }
            loop {
                match inputs.recv_timeout(QUIET) {
                    Ok(Some(root)) => {
                        changed_roots.insert(root);
                    }
                    Ok(None) => full_rescan = true,
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
            let filter = (!full_rescan).then_some(&changed_roots);
            let update = self.scan(filter, false);
            if self.updates.send(update).is_err() {
                return;
            }
        }
    }

    fn scan(&mut self, changed_roots: Option<&BTreeSet<PathBuf>>, initial: bool) -> ShellUpdate {
        let found = discover(&self.sources);
        let active_ids: BTreeSet<String> = found
            .iter()
            .filter(|d| d.duplicate_of.is_none() && !d.manifest.is_theme_only())
            .map(|d| d.manifest.id.clone())
            .collect();

        let mut changes = Vec::new();
        let mut removed_ids = Vec::new();
        let previously_installed: Vec<String> =
            self.plugins.installed().map(str::to_owned).collect();
        for id in previously_installed {
            if !active_ids.contains(&id) {
                changes.push(self.plugins.remove(&id));
                self.compiled_roots.remove(&id);
                removed_ids.push(id);
            }
        }

        let mut loaded = Vec::with_capacity(found.len());
        let mut themes = Vec::new();
        let mut texts_by_plugin = BTreeMap::new();
        for item in found {
            let id = item.manifest.id.clone();
            if let Some(other) = &item.duplicate_of {
                loaded.push(LoadedPluginInfo {
                    id,
                    name: item.manifest.name,
                    version: item.manifest.version,
                    root: item.root,
                    source: item.source,
                    error: Some(format!("与 {} 重复，已跳过", other.display())),
                });
                continue;
            }
            let theme_error = match read_themes(&item.root, &item.manifest.entry) {
                Ok(sources) => {
                    if !sources.is_empty() {
                        themes.push((id.clone(), sources));
                    }
                    None
                }
                Err(error) => Some(error),
            };
            if item.manifest.is_theme_only() {
                loaded.push(LoadedPluginInfo {
                    id,
                    name: item.manifest.name,
                    version: item.manifest.version,
                    root: item.root,
                    source: item.source,
                    error: theme_error,
                });
                continue;
            }
            let unchanged = changed_roots.is_some_and(|changed| {
                !changed.contains(&item.root)
                    && self
                        .compiled_roots
                        .get(&id)
                        .is_some_and(|prev| prev == &item.root)
            });
            let compile_error = if unchanged {
                None
            } else {
                match read_texts(&item.root).and_then(|texts| {
                    let change =
                        self.plugins
                            .install_entry(&id, &item.root, &item.manifest.entry)?;
                    Ok((change, texts))
                }) {
                    Ok((change, texts)) => {
                        texts_by_plugin.insert(id.clone(), texts);
                        self.compiled_roots.insert(id.clone(), item.root.clone());
                        if !initial {
                            changes.push(change);
                        }
                        None
                    }
                    Err(error) => {
                        eprintln!("[smelt-nsh] 插件 {id} 编译失败：{error}");
                        Some(error)
                    }
                }
            };
            loaded.push(LoadedPluginInfo {
                id,
                name: item.manifest.name,
                version: item.manifest.version,
                root: item.root,
                source: item.source,
                error: theme_error.or(compile_error),
            });
        }

        let initial_scripts = if initial {
            changes = vec![self.plugins.start()];
            Some(self.plugins.installed().map(str::to_owned).collect())
        } else {
            None
        };
        ShellUpdate {
            changes,
            removed_ids,
            loaded,
            themes,
            texts: texts_by_plugin,
            initial_scripts,
        }
    }
}

#[derive(Clone)]
struct WatchedDir {
    original: PathBuf,
    canonical: PathBuf,
}

fn watch_sources(
    sources: &[SourceDir],
    inputs: mpsc::Sender<Option<PathBuf>>,
) -> notify::Result<notify::RecommendedWatcher> {
    let dirs: Vec<WatchedDir> = sources
        .iter()
        .filter(|s| s.watch)
        .filter_map(|s| {
            if !s.path.is_dir() {
                let _ = fs::create_dir_all(&s.path);
            }
            let canonical = fs::canonicalize(&s.path).ok()?;
            Some(WatchedDir {
                original: s.path.clone(),
                canonical,
            })
        })
        .collect();
    let matcher = dirs.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else {
            let _ = inputs.send(None);
            return;
        };
        if matches!(event.kind, notify::EventKind::Access(_)) {
            return;
        }
        if event.paths.is_empty() {
            let _ = inputs.send(None);
            return;
        }
        for path in &event.paths {
            for dir in &matcher {
                let rel = path
                    .strip_prefix(&dir.canonical)
                    .or_else(|_| path.strip_prefix(&dir.original));
                let Ok(rel) = rel else {
                    continue;
                };
                let mut parts = rel.components();
                let Some(Component::Normal(plugin_dir)) = parts.next() else {
                    continue;
                };
                if plugin_dir.to_string_lossy().starts_with('.') {
                    continue;
                }
                let ignored = parts.any(|part| {
                    let s = part.as_os_str().to_string_lossy();
                    s.starts_with('.')
                        || s == "node_modules"
                        || s == "gpui-kit.d.ts"
                        || s == "jsconfig.json"
                });
                if !ignored {
                    let _ = inputs.send(Some(dir.original.join(plugin_dir)));
                }
            }
        }
    })?;
    for dir in &dirs {
        watcher.watch(&dir.original, notify::RecursiveMode::Recursive)?;
    }
    Ok(watcher)
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(prefix: &str) -> Self {
            let path = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_plugin(dir: &Path, id: &str, entry: &str, body: &str) {
        fs::create_dir_all(dir).unwrap();
        let manifest = serde_json::json!({
            "id": id,
            "name": id,
            "version": "1.0.0",
            "entry": entry,
        });
        fs::write(dir.join(MANIFEST_FILE), manifest.to_string()).unwrap();
        let entry_path = dir.join(entry);
        if let Some(parent) = entry_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(entry_path, body).unwrap();
    }

    fn wait_update(
        rx: &mpsc::Receiver<ShellUpdate>,
        what: &str,
        pred: impl Fn(&ShellUpdate) -> bool,
    ) -> ShellUpdate {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or_else(|| panic!("等待 {what} 超时"));
            let update = rx
                .recv_timeout(remaining)
                .unwrap_or_else(|err| panic!("等待 {what} 失败：{err}"));
            if pred(&update) {
                return update;
            }
        }
    }

    /// 覆盖 `ShellLive` 热重载完整链路：
    /// 1. 首次扫描加载内置/用户目录，内置同 id 优胜于用户副本，纯主题包只产主题不起脚本 VM；
    /// 2. 改 `alpha` 只增量重编 `alpha`（不重编未改的 `beta`），并校验 `smelt/sdk` -> `smelt/host`；
    /// 3. `alpha` 写入语法错时保留旧版本（不下发 Remove/Install）并记入错误；修复后恢复热重载；
    /// 4. 删除 `alpha` 下发 `Change::Remove("alpha")` 与 `removed_ids`，`beta` 不受影响。
    #[test]
    fn shell_live_hot_reloads_incrementally_and_preserves_siblings() {
        let root = TempDir::new("smelt-nsh-manifest-live");
        let builtin = root.0.join("builtin");
        let user = root.0.join("user");
        fs::create_dir_all(&builtin).unwrap();
        fs::create_dir_all(&user).unwrap();

        let alpha_v1 = r#"
import { View } from "nsh";
import { localOs, SmeltIcon } from "smelt/sdk";
export function Main() {
  void localOs();
  return <View><SmeltIcon path="icons/plus.svg" size={14} /></View>;
}
"#;
        let beta_v1 = r#"
import { Text } from "nsh";
export function Main() {
  return <Text>beta</Text>;
}
"#;
        write_plugin(&builtin.join("alpha"), "alpha", "main.tsx", alpha_v1);
        write_plugin(&builtin.join("beta"), "beta", "main.tsx", beta_v1);
        // 用户目录里放一份与内置同 id 的副本：应被跳过并报重复错误。
        write_plugin(&user.join("alpha-dup"), "alpha", "main.tsx", beta_v1);
        // 纯主题插件：只收主题 JSON，不起脚本 VM。
        write_plugin(
            &user.join("theme-pack"),
            "theme-pack",
            "themes/custom.json",
            r#"{"name":"Custom"}"#,
        );

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (sync_tx, sync_rx) = mpsc::channel();
        std::thread::spawn(move || {
            while let Some(update) = rx.blocking_recv() {
                if sync_tx.send(update).is_err() {
                    break;
                }
            }
        });

        let _live = ShellLive::start(
            vec![
                SourceDir {
                    path: builtin.clone(),
                    source: PluginSource::Builtin,
                    watch: true,
                },
                SourceDir {
                    path: user.clone(),
                    source: PluginSource::User,
                    watch: true,
                },
            ],
            root.0.join("stage"),
            "export function Shell() { return null; }",
            crate::ui_sets(),
            tx,
        );

        let initial = wait_update(&sync_rx, "首次扫描", |u| u.initial_scripts.is_some());
        assert_eq!(initial.changes.len(), 1);
        match &initial.changes[0] {
            Change::Start { plugins, .. } => {
                let ids: Vec<&str> = plugins.iter().map(|p| p.id()).collect();
                assert_eq!(ids, vec!["alpha", "beta"]);
            }
            other => panic!("首次扫描应下发 Change::Start，收到 {other:?}"),
        }
        assert_eq!(initial.themes.len(), 1);
        assert_eq!(initial.themes[0].0, "theme-pack");
        assert!(
            initial
                .loaded
                .iter()
                .any(|p| p.id == "alpha" && p.source == PluginSource::User && p.error.is_some()),
            "用户同 id 副本应记为重复错误"
        );
        assert!(
            initial
                .loaded
                .iter()
                .any(|p| p.id == "alpha" && p.source == PluginSource::Builtin && p.error.is_none()),
            "内置 alpha 应正常加载"
        );

        // 改动 alpha：应仅增量重编 alpha，不触碰 beta。
        let alpha_v2 = alpha_v1.replace("icons/plus.svg", "icons/x.svg");
        fs::write(builtin.join("alpha/main.tsx"), &alpha_v2).unwrap();
        let edited = wait_update(&sync_rx, "增量重编 alpha", |u| {
            u.changes
                .iter()
                .any(|c| matches!(c, Change::Install(p) if p.id() == "alpha"))
        });
        assert_eq!(edited.changes.len(), 1, "只应重编改动的 alpha，不动 beta");
        assert!(edited.removed_ids.is_empty());

        // alpha 写入语法错误：不卸载旧版本，只记录编译错误。
        fs::write(builtin.join("alpha/main.tsx"), "export function (").unwrap();
        let broken = wait_update(&sync_rx, "alpha 语法错", |u| {
            u.loaded
                .iter()
                .any(|p| p.id == "alpha" && p.source == PluginSource::Builtin && p.error.is_some())
        });
        assert!(
            broken.changes.is_empty(),
            "语法错不应卸载或重装上一份正常插件：{:?}",
            broken.changes
        );

        // 修复 alpha：恢复正常热重载。
        fs::write(builtin.join("alpha/main.tsx"), &alpha_v2).unwrap();
        let fixed = wait_update(&sync_rx, "修复 alpha", |u| {
            u.changes
                .iter()
                .any(|c| matches!(c, Change::Install(p) if p.id() == "alpha"))
                && u.loaded.iter().any(|p| {
                    p.id == "alpha" && p.source == PluginSource::Builtin && p.error.is_none()
                })
        });
        assert_eq!(fixed.changes.len(), 1);

        // 删除内置 alpha 与用户重复副本：下发 Remove("alpha")，beta 仍在。
        fs::remove_dir_all(builtin.join("alpha")).unwrap();
        fs::remove_dir_all(user.join("alpha-dup")).unwrap();
        let removed = wait_update(&sync_rx, "删除 alpha", |u| {
            u.removed_ids.iter().any(|id| id == "alpha")
        });
        assert!(
            removed
                .changes
                .iter()
                .any(|c| matches!(c, Change::Remove(id) if id == "alpha"))
        );
        assert!(
            removed
                .loaded
                .iter()
                .any(|p| p.id == "beta" && p.error.is_none()),
            "删除 alpha 后 beta 应保持正常"
        );
    }
}
