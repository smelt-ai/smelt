//! 主壳的插件发现、清单读取与多目录热重载（`smelt-plugin.json`）。
//!
//! 扫描两处目录：
//! 1. App 自带插件目录（开发态读仓库 `plugins/`，发版 `.app` 读 `Contents/Resources/plugins/`）；
//! 2. 用户插件目录（`~/.smelt/plugins/`）。
//!
//! 规则：同来源内重复 `id` 记为错误跳过；用户插件可与自带插件同 `id` 构成覆写（用户优先），
//! 当用户覆写版加载、主题校验、编译或启动失败时自动回退到自带版本并在状态里保留错误原因；
//! 插件可附带 `themes/*.json`（或 JSON entry 的纯主题包），统一写入主题目录；
//! 监听目录（开发态含自带目录，生产态仅用户目录）文件变化后防抖 200ms 仅重编改动的插件。

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
    sync::mpsc::{self, RecvTimeoutError},
    time::{Duration, Instant},
};

use notify::Watcher as _;
use nsh_core::element::description::SetDescription;
use tokio::sync::mpsc::UnboundedSender;

use crate::plugins::{Change, Compiled, Plugins};

/// NSH 插件清单文件名。
pub const MANIFEST_FILE: &str = "smelt-plugin.json";
const PRECOMPILED_BYTECODE_FILE: &str = "bundle.nshc";
const PRECOMPILED_META_FILE: &str = "bundle.meta.json";

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct PrecompiledMeta {
    bytecode_version: String,
    sdk_fingerprint: String,
    entry: String,
}

pub(crate) enum ShellSource {
    #[cfg_attr(not(test), allow(dead_code))]
    Source(&'static str),
    Bytecode(&'static [u8]),
}

/// 预编译内置插件目录的汇报（供 `smelt plugin stage` 与 `scripts/stage-plugins.sh` 输出）。
#[derive(Clone, Debug, serde::Serialize)]
pub struct StageReport {
    pub total: usize,
    pub script_plugins: usize,
    pub theme_only: usize,
    pub elapsed_ms: u64,
}

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
        if !crate::plugins::valid_id(&manifest.id) {
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

/// 离线检查一个插件的详细报告（供 `smelt plugin check` 输出）。
#[derive(Clone, Debug)]
pub struct PluginCheckReport {
    pub manifest: PluginManifest,
    pub theme_only: bool,
    pub themes: Vec<String>,
    pub exports: Vec<String>,
}

/// 检查插件目录是否合法（清单、文案、主题、TSX 打包、import、未声明标识符与 QuickJS 字节码编译）。
pub fn check_plugin(
    root: &Path,
    ui_sets: Vec<SetDescription>,
) -> Result<PluginCheckReport, String> {
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
    let normalized_entry: PathBuf = entry_path
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect();
    if !root.join(&normalized_entry).is_file() {
        return Err(format!("插件入口 {} 不存在", manifest.entry));
    }
    read_texts(root)?;
    let themes = read_themes(root, &manifest.entry)?;
    let theme_names = validate_themes(&manifest.id, &themes)?;
    if manifest.is_theme_only() {
        return Ok(PluginCheckReport {
            manifest,
            theme_only: true,
            themes: theme_names,
            exports: Vec::new(),
        });
    }
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let stage = std::env::temp_dir().join(format!("smelt-nsh-check-{}", uuid::Uuid::new_v4()));
    let _cleanup = Cleanup(stage.clone());
    let compiled_code = Plugins::compile_source_to_js(
        &stage,
        &ui_sets,
        &manifest.id,
        root,
        &manifest.entry,
        false,
    )?;
    crate::plugins::validate_unresolved_references(&manifest.id, &compiled_code)?;
    nsh_js::compile_bytecode_with(&compiled_code, nsh_js::Strip::Source, &ui_sets)
        .map_err(|error| format!("{} QuickJS 编译失败：{}", manifest.id, error.message))?;
    let exports = crate::plugins::extract_entry_exports(&root.join(&normalized_entry));
    Ok(PluginCheckReport {
        manifest,
        theme_only: false,
        themes: theme_names,
        exports,
    })
}

fn worker_threads(count: usize) -> usize {
    let cap = std::thread::available_parallelism()
        .map_or(2, |n| n.get())
        .min(4);
    count.clamp(1, cap)
}

fn normalize_relative_entry(entry: &str) -> Result<PathBuf, String> {
    let path = Path::new(entry);
    if path.is_absolute()
        || path.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(format!("插件入口必须是插件目录内的相对路径：{entry}"));
    }
    Ok(path
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect())
}

fn copy_dir_filtered(src: &Path, dst: &Path) -> Result<(), String> {
    if !src.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(dst).map_err(|e| format!("创建 {} 失败：{e}", dst.display()))?;
    for entry in fs::read_dir(src).map_err(|e| format!("读取 {} 失败：{e}", src.display()))? {
        let entry = entry.map_err(|e| format!("读取 {} 项失败：{e}", src.display()))?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') || name_str == "node_modules" || name_str == "tsconfig.json" {
            continue;
        }
        let kind = entry
            .file_type()
            .map_err(|e| format!("检查 {} 类型失败：{e}", entry.path().display()))?;
        if kind.is_dir() && name_str == "tests" {
            continue;
        }
        if kind.is_file()
            && (name_str.contains(".test.")
                || name_str == PRECOMPILED_BYTECODE_FILE
                || name_str == PRECOMPILED_META_FILE)
        {
            continue;
        }
        let target = dst.join(&name);
        if kind.is_dir() {
            copy_dir_filtered(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &target).map_err(|e| {
                format!(
                    "拷贝 {} -> {} 失败：{e}",
                    entry.path().display(),
                    target.display()
                )
            })?;
        }
    }
    Ok(())
}

/// 将仓库自带插件目录 `src_root` 预编译打包到 `dst_root`：
/// - 脚本插件生成 `bundle.nshc`（QuickJS 字节码，保留行号表）与 `bundle.meta.json`，同时保留插件
///   源码与 `locales/`、`themes/`（跳过 dotfiles、`node_modules`、`tsconfig.json`、`tests/`、
///   `*.test.*` 与软链），兼顾发版包零打包冷启与 `smelt plugin fork` 离线拷贝；
/// - 纯主题插件拷贝清单、文案、主题 json。
pub fn stage_builtin_plugins(src_root: &Path, dst_root: &Path) -> Result<StageReport, String> {
    let t0 = Instant::now();
    let ui_sets = crate::ui_sets();
    let entries = fs::read_dir(src_root)
        .map_err(|e| format!("读取内置插件目录 {} 失败：{e}", src_root.display()))?;
    let mut roots: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .map(|entry| entry.path())
        .filter(|path| path.join(MANIFEST_FILE).is_file())
        .collect();
    roots.sort();

    struct StageItem {
        dir_name: std::ffi::OsString,
        root: PathBuf,
        manifest: PluginManifest,
    }

    let mut seen_ids = BTreeSet::new();
    let mut items = Vec::with_capacity(roots.len());
    for root in roots {
        let manifest = PluginManifest::read(&root)?;
        if !seen_ids.insert(manifest.id.clone()) {
            return Err(format!("内置插件目录存在重复 id：{}", manifest.id));
        }
        let normalized_entry = normalize_relative_entry(&manifest.entry)?;
        if !root.join(&normalized_entry).is_file() {
            return Err(format!(
                "插件 {} 入口 {} 不存在",
                manifest.id, manifest.entry
            ));
        }
        read_texts(&root)?;
        let themes = read_themes(&root, &manifest.entry)?;
        validate_themes(&manifest.id, &themes)?;
        let dir_name = root
            .file_name()
            .ok_or_else(|| format!("非法插件目录：{}", root.display()))?
            .to_os_string();
        items.push(StageItem {
            dir_name,
            root,
            manifest,
        });
    }

    let dst_parent = dst_root
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dst_parent)
        .map_err(|e| format!("创建目标父目录 {} 失败：{e}", dst_parent.display()))?;

    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    let suffix = uuid::Uuid::new_v4();
    let staged_out = dst_parent.join(format!(".smelt-stage-out-{suffix}"));
    let compile_stage = std::env::temp_dir().join(format!("smelt-stage-work-{suffix}"));
    let _out_cleanup = Cleanup(staged_out.clone());
    let _work_cleanup = Cleanup(compile_stage.clone());
    fs::create_dir_all(&staged_out)
        .map_err(|e| format!("创建暂存目录 {} 失败：{e}", staged_out.display()))?;

    let sdk_fp = crate::plugins::sdk_fingerprint();
    let mut theme_only_count = 0usize;
    let mut script_items: Vec<&StageItem> = Vec::new();
    for item in &items {
        let plugin_dst = staged_out.join(&item.dir_name);
        copy_dir_filtered(&item.root, &plugin_dst)?;

        if item.manifest.is_theme_only() {
            theme_only_count += 1;
        } else {
            script_items.push(item);
        }
    }

    if !script_items.is_empty() {
        for res in run_parallel(&script_items, |item| -> Result<(), String> {
            let id = &item.manifest.id;
            let code = Plugins::compile_source_to_js(
                &compile_stage,
                &ui_sets,
                id,
                &item.root,
                &item.manifest.entry,
                true,
            )?;
            crate::plugins::validate_unresolved_references(id, &code)?;
            let bytecode = nsh_js::compile_bytecode_with(&code, nsh_js::Strip::Source, &ui_sets)
                .map_err(|err| format!("{id} QuickJS 编译失败：{}", err.message))?;
            let plugin_dst = staged_out.join(&item.dir_name);
            fs::write(plugin_dst.join(PRECOMPILED_BYTECODE_FILE), &bytecode)
                .map_err(|e| format!("写入 {id} 字节码失败：{e}"))?;
            let meta = PrecompiledMeta {
                bytecode_version: nsh_js::BYTECODE_VERSION.to_owned(),
                sdk_fingerprint: sdk_fp.clone(),
                entry: item.manifest.entry.clone(),
            };
            let meta_json = serde_json::to_string_pretty(&meta)
                .map_err(|e| format!("序列化 {id} 元数据失败：{e}"))?;
            fs::write(plugin_dst.join(PRECOMPILED_META_FILE), meta_json)
                .map_err(|e| format!("写入 {id} 元数据失败：{e}"))?;
            Ok(())
        }) {
            res?;
        }
    }

    if dst_root.exists() {
        fs::remove_dir_all(dst_root)
            .map_err(|e| format!("清理旧目录 {} 失败：{e}", dst_root.display()))?;
    }
    fs::rename(&staged_out, dst_root)
        .map_err(|e| format!("落位目录 {} 失败：{e}", dst_root.display()))?;

    let script_count = script_items.len();
    Ok(StageReport {
        total: items.len(),
        script_plugins: script_count,
        theme_only: theme_only_count,
        elapsed_ms: t0.elapsed().as_millis() as u64,
    })
}

/// 暴露给设置「面板插件」页与 `smelt plugin status` 的插件状态快照。
#[derive(Clone, Debug)]
pub struct LoadedPluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub root: PathBuf,
    pub source: PluginSource,
    pub overrides_builtin: bool,
    pub fallback_to_builtin: bool,
    pub views: Vec<String>,
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

fn validate_themes(id: &str, themes: &[String]) -> Result<Vec<String>, String> {
    if themes.is_empty() {
        return Ok(Vec::new());
    }
    let refs: Vec<&str> = themes.iter().map(String::as_str).collect();
    let mut catalog = smelt_ui::theme_catalog::ThemeCatalog::default();
    catalog
        .register(
            &format!("files:{id}"),
            &refs,
            smelt_ui::theme_catalog::is_reserved,
        )
        .map_err(|error| error.to_string())
        .map(|names| names.into_iter().map(|n| n.to_string()).collect())
}

#[derive(Clone, Debug)]
struct Discovered {
    root: PathBuf,
    manifest: PluginManifest,
    source: PluginSource,
    watch: bool,
}

enum DiscoverySlot {
    Active {
        primary: Discovered,
        fallback: Option<Discovered>,
    },
    Duplicate {
        item: Discovered,
        duplicate_of: PathBuf,
    },
}

fn read_source_manifests(sources: &[SourceDir], target: PluginSource) -> Vec<Discovered> {
    let mut out = Vec::new();
    for src in sources.iter().filter(|s| s.source == target) {
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
                Ok(manifest) => out.push(Discovered {
                    root,
                    manifest,
                    source: src.source,
                    watch: src.watch,
                }),
                Err(error) => eprintln!("[smelt-nsh] {error}"),
            }
        }
    }
    out
}

fn discover_slots(sources: &[SourceDir]) -> Vec<DiscoverySlot> {
    let mut slots: Vec<DiscoverySlot> = Vec::new();
    let mut builtin_ix: BTreeMap<String, usize> = BTreeMap::new();
    let mut seen_builtin: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut duplicates: Vec<DiscoverySlot> = Vec::new();

    for item in read_source_manifests(sources, PluginSource::Builtin) {
        let id = item.manifest.id.clone();
        if let Some(first_root) = seen_builtin.get(&id) {
            duplicates.push(DiscoverySlot::Duplicate {
                item,
                duplicate_of: first_root.clone(),
            });
            continue;
        }
        seen_builtin.insert(id.clone(), item.root.clone());
        let ix = slots.len();
        slots.push(DiscoverySlot::Active {
            primary: item,
            fallback: None,
        });
        builtin_ix.insert(id, ix);
    }

    let mut seen_user: BTreeMap<String, PathBuf> = BTreeMap::new();
    for item in read_source_manifests(sources, PluginSource::User) {
        let id = item.manifest.id.clone();
        if let Some(first_root) = seen_user.get(&id) {
            duplicates.push(DiscoverySlot::Duplicate {
                item,
                duplicate_of: first_root.clone(),
            });
            continue;
        }
        seen_user.insert(id.clone(), item.root.clone());
        if let Some(&ix) = builtin_ix.get(&id) {
            let DiscoverySlot::Active { primary, fallback } = &mut slots[ix] else {
                unreachable!();
            };
            let builtin_item = std::mem::replace(primary, item);
            *fallback = Some(builtin_item);
        } else {
            slots.push(DiscoverySlot::Active {
                primary: item,
                fallback: None,
            });
        }
    }

    slots.extend(duplicates);
    slots
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

fn run_parallel<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    if items.is_empty() {
        return Vec::new();
    }
    if items.len() == 1 {
        return vec![f(&items[0])];
    }
    let workers = worker_threads(items.len());
    let next_ix = std::sync::atomic::AtomicUsize::new(0);
    let collected = std::sync::Mutex::new(Vec::with_capacity(items.len()));
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                loop {
                    let ix = next_ix.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(item) = items.get(ix) else {
                        return;
                    };
                    let res = f(item);
                    collected.lock().unwrap().push((ix, res));
                }
            });
        }
    });
    let mut slots: Vec<Option<R>> = (0..items.len()).map(|_| None).collect();
    for (ix, res) in collected.into_inner().unwrap() {
        slots[ix] = Some(res);
    }
    slots.into_iter().map(Option::unwrap).collect()
}

fn try_load_precompiled(item: &Discovered, sdk_fp: &str) -> Option<Compiled> {
    if item.source != PluginSource::Builtin || item.watch {
        return None;
    }
    let meta_path = item.root.join(PRECOMPILED_META_FILE);
    let bytecode_path = item.root.join(PRECOMPILED_BYTECODE_FILE);
    if !meta_path.is_file() || !bytecode_path.is_file() {
        return None;
    }
    let meta_raw = match fs::read_to_string(&meta_path) {
        Ok(s) => s,
        Err(err) => {
            eprintln!(
                "[smelt-nsh] 读取 {} 失败，回退源码编译：{err}",
                meta_path.display()
            );
            return None;
        }
    };
    let meta: PrecompiledMeta = match serde_json::from_str(&meta_raw) {
        Ok(m) => m,
        Err(err) => {
            eprintln!(
                "[smelt-nsh] 解析 {} 失败，回退源码编译：{err}",
                meta_path.display()
            );
            return None;
        }
    };
    if meta.bytecode_version != nsh_js::BYTECODE_VERSION
        || meta.sdk_fingerprint != sdk_fp
        || meta.entry != item.manifest.entry
    {
        eprintln!(
            "[smelt-nsh] 插件 {} 预编译元数据不匹配（version={} vs {}, sdk={} vs {}, entry={} vs {}），回退源码编译",
            item.manifest.id,
            meta.bytecode_version,
            nsh_js::BYTECODE_VERSION,
            meta.sdk_fingerprint,
            sdk_fp,
            meta.entry,
            item.manifest.entry,
        );
        return None;
    }
    match fs::read(&bytecode_path) {
        Ok(bytes) if !bytes.is_empty() => {
            Some(Compiled::from_bytecode(item.manifest.id.clone(), bytes))
        }
        Ok(_) => {
            eprintln!(
                "[smelt-nsh] 插件 {} 预编译字节码为空，回退源码编译",
                item.manifest.id
            );
            None
        }
        Err(err) => {
            eprintln!(
                "[smelt-nsh] 读取 {} 失败，回退源码编译：{err}",
                bytecode_path.display()
            );
            None
        }
    }
}

pub(crate) enum LiveInput {
    Changed(Option<PathBuf>),
    FallbackBuiltin { id: String, reason: String },
}

/// 后台发现与热重载句柄：释放后自动停止文件监听与后台线程。
pub(crate) struct ShellLive {
    inputs: mpsc::Sender<LiveInput>,
    _watcher: Option<notify::RecommendedWatcher>,
}

impl ShellLive {
    pub(crate) fn start(
        sources: Vec<SourceDir>,
        stage: PathBuf,
        shell: ShellSource,
        ui_sets: Vec<SetDescription>,
        updates: UnboundedSender<ShellUpdate>,
    ) -> Self {
        let (inputs, received) = mpsc::channel();
        let watcher = watch_sources(&sources, inputs.clone())
            .inspect_err(|error| eprintln!("[smelt-nsh] 监听插件目录失败：{error}"))
            .ok()
            .flatten();
        std::thread::Builder::new()
            .name("smelt-nsh-shell-live".into())
            .spawn(move || {
                let plugins_res = match shell {
                    ShellSource::Bytecode(bytes) => {
                        Plugins::new_with_bytecode(&stage, bytes, ui_sets)
                    }
                    ShellSource::Source(src) => Plugins::new(&stage, src, ui_sets),
                };
                match plugins_res {
                    Ok(plugins) => Worker {
                        sources,
                        plugins,
                        updates,
                        compiled_roots: BTreeMap::new(),
                        failed_user_overrides: BTreeMap::new(),
                    }
                    .run(&received),
                    Err(error) => eprintln!("[smelt-nsh] 主壳打包失败：{error}"),
                }
                if stage.exists()
                    && let Err(error) = fs::remove_dir_all(&stage)
                {
                    eprintln!("[smelt-nsh] 清理暂存目录 {} 失败：{error}", stage.display());
                }
            })
            .expect("spawn smelt-nsh shell live worker");
        Self {
            inputs,
            _watcher: watcher,
        }
    }

    pub(crate) fn request_builtin_fallback(&self, id: String, reason: String) {
        let _ = self.inputs.send(LiveInput::FallbackBuiltin { id, reason });
    }
}

struct Worker {
    sources: Vec<SourceDir>,
    plugins: Plugins,
    updates: UnboundedSender<ShellUpdate>,
    /// 插件 id → 当前已装进引擎的目录路径（用于按目录增量重编及覆写/回退切换）。
    compiled_roots: BTreeMap<String, PathBuf>,
    /// 编译/校验/启动失败且已回退到自带版本的用户覆写插件：`id -> (failed_user_root, error)`。
    failed_user_overrides: BTreeMap<String, (PathBuf, String)>,
}

impl Worker {
    fn run(&mut self, inputs: &mpsc::Receiver<LiveInput>) {
        let initial = self.scan(None, true, BTreeMap::new(), true);
        if self.updates.send(initial).is_err() {
            return;
        }
        while let Ok(first) = inputs.recv() {
            let mut changed_roots = BTreeSet::new();
            let mut full_rescan = false;
            let mut fallback_requests = BTreeMap::new();
            let push = |ev: LiveInput,
                        changed: &mut BTreeSet<PathBuf>,
                        full: &mut bool,
                        fb: &mut BTreeMap<String, String>| match ev {
                LiveInput::Changed(Some(root)) => {
                    changed.insert(root);
                }
                LiveInput::Changed(None) => *full = true,
                LiveInput::FallbackBuiltin { id, reason } => {
                    fb.insert(id, reason);
                }
            };
            let wait_quiet = !matches!(first, LiveInput::FallbackBuiltin { .. });
            push(
                first,
                &mut changed_roots,
                &mut full_rescan,
                &mut fallback_requests,
            );
            if wait_quiet {
                loop {
                    match inputs.recv_timeout(QUIET) {
                        Ok(ev) => push(
                            ev,
                            &mut changed_roots,
                            &mut full_rescan,
                            &mut fallback_requests,
                        ),
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
            } else {
                while let Ok(ev) = inputs.try_recv() {
                    push(
                        ev,
                        &mut changed_roots,
                        &mut full_rescan,
                        &mut fallback_requests,
                    );
                }
            }
            let filter = (!full_rescan).then_some(&changed_roots);
            let update = self.scan(filter, full_rescan, fallback_requests, false);
            if self.updates.send(update).is_err() {
                return;
            }
        }
    }

    fn scan(
        &mut self,
        changed_roots: Option<&BTreeSet<PathBuf>>,
        full_rescan: bool,
        mut fallback_requests: BTreeMap<String, String>,
        initial: bool,
    ) -> ShellUpdate {
        if full_rescan {
            self.failed_user_overrides.clear();
        } else if let Some(changed) = changed_roots {
            self.failed_user_overrides
                .retain(|_, (failed_root, _)| !changed.contains(failed_root));
        }

        let slots = discover_slots(&self.sources);
        let sdk_fp = crate::plugins::sdk_fingerprint();

        struct CompileJob {
            slot_ix: usize,
            id: String,
            root: PathBuf,
            entry: String,
        }

        struct PendingFallback {
            slot_ix: usize,
            primary: Discovered,
            fallback: Discovered,
            reason: String,
        }

        enum SlotStage {
            Done {
                info: LoadedPluginInfo,
                themes: Option<(String, Vec<String>)>,
                texts: Option<(String, serde_json::Value)>,
                install: Option<Compiled>,
                keep_script_id: Option<String>,
            },
            WaitingPrimaryCompile {
                primary: Discovered,
                fallback: Option<Discovered>,
                themes: Vec<String>,
                texts: serde_json::Value,
                theme_error: Option<String>,
            },
            WaitingFallbackCompile {
                info: LoadedPluginInfo,
                fb_root: PathBuf,
                fb_themes: Option<Vec<String>>,
                fb_texts: serde_json::Value,
            },
        }

        let mut stages: Vec<Option<SlotStage>> = Vec::with_capacity(slots.len());
        let mut primary_jobs: Vec<CompileJob> = Vec::new();
        let mut pending_fallbacks: Vec<PendingFallback> = Vec::new();

        for (slot_ix, slot) in slots.into_iter().enumerate() {
            match slot {
                DiscoverySlot::Duplicate { item, duplicate_of } => {
                    stages.push(Some(SlotStage::Done {
                        info: LoadedPluginInfo {
                            id: item.manifest.id,
                            name: item.manifest.name,
                            version: item.manifest.version,
                            root: item.root,
                            source: item.source,
                            overrides_builtin: false,
                            fallback_to_builtin: false,
                            views: Vec::new(),
                            error: Some(format!("与 {} 重复，已跳过", duplicate_of.display())),
                        },
                        themes: None,
                        texts: None,
                        install: None,
                        keep_script_id: None,
                    }));
                }
                DiscoverySlot::Active { primary, fallback } => {
                    let id = primary.manifest.id.clone();
                    let overrides_builtin = fallback.is_some();

                    let forced_reason = if let Some(reason) = fallback_requests.remove(&id)
                        && fallback.is_some()
                    {
                        self.failed_user_overrides
                            .insert(id.clone(), (primary.root.clone(), reason.clone()));
                        Some(reason)
                    } else if fallback.is_some() {
                        match self.failed_user_overrides.get(&id) {
                            Some((failed_root, prev_reason)) if failed_root == &primary.root => {
                                Some(prev_reason.clone())
                            }
                            _ => {
                                self.failed_user_overrides.remove(&id);
                                None
                            }
                        }
                    } else {
                        self.failed_user_overrides.remove(&id);
                        None
                    };

                    if let (Some(reason), Some(fb)) = (forced_reason, fallback.as_ref()) {
                        pending_fallbacks.push(PendingFallback {
                            slot_ix,
                            primary,
                            fallback: fb.clone(),
                            reason,
                        });
                        stages.push(None);
                        continue;
                    }

                    let (themes, theme_error) =
                        match read_themes(&primary.root, &primary.manifest.entry).and_then(
                            |sources| {
                                validate_themes(&id, &sources)?;
                                Ok(sources)
                            },
                        ) {
                            Ok(sources) => (sources, None),
                            Err(err) => (Vec::new(), Some(err)),
                        };

                    if let (Some(err), Some(fb)) = (&theme_error, fallback.as_ref()) {
                        eprintln!(
                            "[smelt-nsh] 用户覆写插件 {id} 失败，已自动回退到自带版本：{err}"
                        );
                        pending_fallbacks.push(PendingFallback {
                            slot_ix,
                            reason: err.clone(),
                            primary,
                            fallback: fb.clone(),
                        });
                        stages.push(None);
                        continue;
                    }

                    if primary.manifest.is_theme_only() {
                        stages.push(Some(SlotStage::Done {
                            info: LoadedPluginInfo {
                                id: id.clone(),
                                name: primary.manifest.name,
                                version: primary.manifest.version,
                                root: primary.root,
                                source: primary.source,
                                overrides_builtin,
                                fallback_to_builtin: false,
                                views: Vec::new(),
                                error: theme_error,
                            },
                            themes: (!themes.is_empty()).then_some((id, themes)),
                            texts: None,
                            install: None,
                            keep_script_id: None,
                        }));
                        continue;
                    }

                    let unchanged = !full_rescan
                        && changed_roots.is_some_and(|changed| {
                            !changed.contains(&primary.root)
                                && self
                                    .compiled_roots
                                    .get(&id)
                                    .is_some_and(|prev| prev == &primary.root)
                        });
                    if unchanged {
                        stages.push(Some(SlotStage::Done {
                            info: LoadedPluginInfo {
                                id: id.clone(),
                                name: primary.manifest.name,
                                version: primary.manifest.version,
                                root: primary.root,
                                source: primary.source,
                                overrides_builtin,
                                fallback_to_builtin: false,
                                views: Vec::new(),
                                error: theme_error,
                            },
                            themes: (!themes.is_empty()).then_some((id.clone(), themes)),
                            texts: None,
                            install: None,
                            keep_script_id: Some(id),
                        }));
                        continue;
                    }

                    let texts = match read_texts(&primary.root) {
                        Ok(texts) => texts,
                        Err(error) => {
                            if let Some(fb) = fallback {
                                eprintln!(
                                    "[smelt-nsh] 用户覆写插件 {id} 失败，已自动回退到自带版本：{error}"
                                );
                                pending_fallbacks.push(PendingFallback {
                                    slot_ix,
                                    primary,
                                    fallback: fb,
                                    reason: error,
                                });
                                stages.push(None);
                            } else {
                                eprintln!("[smelt-nsh] 插件 {id} 编译失败：{error}");
                                stages.push(Some(SlotStage::Done {
                                    info: LoadedPluginInfo {
                                        id: id.clone(),
                                        name: primary.manifest.name,
                                        version: primary.manifest.version,
                                        root: primary.root,
                                        source: primary.source,
                                        overrides_builtin: false,
                                        fallback_to_builtin: false,
                                        views: Vec::new(),
                                        error: theme_error.or(Some(error)),
                                    },
                                    themes: (!themes.is_empty()).then_some((id.clone(), themes)),
                                    texts: None,
                                    install: None,
                                    keep_script_id: Some(id),
                                }));
                            }
                            continue;
                        }
                    };

                    if let Some(compiled) = try_load_precompiled(&primary, &sdk_fp) {
                        self.compiled_roots.insert(id.clone(), primary.root.clone());
                        stages.push(Some(SlotStage::Done {
                            info: LoadedPluginInfo {
                                id: id.clone(),
                                name: primary.manifest.name,
                                version: primary.manifest.version,
                                root: primary.root,
                                source: primary.source,
                                overrides_builtin,
                                fallback_to_builtin: false,
                                views: Vec::new(),
                                error: theme_error,
                            },
                            themes: (!themes.is_empty()).then_some((id.clone(), themes)),
                            texts: Some((id.clone(), texts)),
                            install: Some(compiled),
                            keep_script_id: Some(id),
                        }));
                    } else {
                        primary_jobs.push(CompileJob {
                            slot_ix,
                            id,
                            root: primary.root.clone(),
                            entry: primary.manifest.entry.clone(),
                        });
                        stages.push(Some(SlotStage::WaitingPrimaryCompile {
                            primary,
                            fallback,
                            themes,
                            texts,
                            theme_error,
                        }));
                    }
                }
            }
        }

        if !primary_jobs.is_empty() {
            let stage_dir = self.plugins.stage();
            let plugins_stage = stage_dir.join("plugins");
            let compiled_results: Vec<Result<Compiled, String>> =
                if let Err(err) = fs::create_dir_all(&plugins_stage) {
                    let msg = format!("创建插件暂存目录 {} 失败：{err}", plugins_stage.display());
                    primary_jobs.iter().map(|_| Err(msg.clone())).collect()
                } else {
                    let ui_sets = self.plugins.ui_sets();
                    run_parallel(&primary_jobs, |job| {
                        let code = Plugins::compile_source_to_js(
                            stage_dir, ui_sets, &job.id, &job.root, &job.entry, false,
                        )?;
                        crate::plugins::validate_unresolved_references(&job.id, &code)?;
                        Ok(Compiled::from_source(job.id.clone(), code))
                    })
                };

            for (job, res) in primary_jobs.into_iter().zip(compiled_results) {
                let Some(SlotStage::WaitingPrimaryCompile {
                    primary,
                    fallback,
                    themes,
                    texts,
                    theme_error,
                }) = stages[job.slot_ix].take()
                else {
                    unreachable!();
                };
                let id = job.id;
                match res {
                    Ok(compiled) => {
                        self.compiled_roots.insert(id.clone(), primary.root.clone());
                        stages[job.slot_ix] = Some(SlotStage::Done {
                            info: LoadedPluginInfo {
                                id: id.clone(),
                                name: primary.manifest.name,
                                version: primary.manifest.version,
                                root: primary.root,
                                source: primary.source,
                                overrides_builtin: fallback.is_some(),
                                fallback_to_builtin: false,
                                views: Vec::new(),
                                error: theme_error,
                            },
                            themes: (!themes.is_empty()).then_some((id.clone(), themes)),
                            texts: Some((id.clone(), texts)),
                            install: Some(compiled),
                            keep_script_id: Some(id),
                        });
                    }
                    Err(error) => {
                        if let Some(fb) = fallback {
                            eprintln!(
                                "[smelt-nsh] 用户覆写插件 {id} 失败，已自动回退到自带版本：{error}"
                            );
                            pending_fallbacks.push(PendingFallback {
                                slot_ix: job.slot_ix,
                                primary,
                                fallback: fb,
                                reason: error,
                            });
                        } else {
                            eprintln!("[smelt-nsh] 插件 {id} 编译失败：{error}");
                            stages[job.slot_ix] = Some(SlotStage::Done {
                                info: LoadedPluginInfo {
                                    id: id.clone(),
                                    name: primary.manifest.name,
                                    version: primary.manifest.version,
                                    root: primary.root,
                                    source: primary.source,
                                    overrides_builtin: false,
                                    fallback_to_builtin: false,
                                    views: Vec::new(),
                                    error: theme_error.or(Some(error)),
                                },
                                themes: (!themes.is_empty()).then_some((id.clone(), themes)),
                                texts: None,
                                install: None,
                                keep_script_id: Some(id),
                            });
                        }
                    }
                }
            }
        }

        let mut fallback_jobs: Vec<CompileJob> = Vec::new();
        for pending in pending_fallbacks {
            let id = pending.primary.manifest.id.clone();
            self.failed_user_overrides.insert(
                id.clone(),
                (pending.primary.root.clone(), pending.reason.clone()),
            );
            let info = LoadedPluginInfo {
                id: id.clone(),
                name: pending.primary.manifest.name,
                version: pending.primary.manifest.version,
                root: pending.primary.root,
                source: PluginSource::User,
                overrides_builtin: true,
                fallback_to_builtin: true,
                views: Vec::new(),
                error: Some(pending.reason),
            };
            let fb = pending.fallback;
            let fb_themes = read_themes(&fb.root, &fb.manifest.entry)
                .and_then(|sources| {
                    validate_themes(&id, &sources)?;
                    Ok(sources)
                })
                .ok()
                .filter(|sources| !sources.is_empty());

            if fb.manifest.is_theme_only() {
                stages[pending.slot_ix] = Some(SlotStage::Done {
                    info,
                    themes: fb_themes.map(|s| (id, s)),
                    texts: None,
                    install: None,
                    keep_script_id: None,
                });
                continue;
            }

            let fb_unchanged = !full_rescan
                && changed_roots.is_some_and(|changed| {
                    !changed.contains(&fb.root)
                        && self
                            .compiled_roots
                            .get(&id)
                            .is_some_and(|prev| prev == &fb.root)
                });
            if fb_unchanged {
                stages[pending.slot_ix] = Some(SlotStage::Done {
                    info,
                    themes: fb_themes.map(|s| (id.clone(), s)),
                    texts: None,
                    install: None,
                    keep_script_id: Some(id),
                });
                continue;
            }

            let fb_texts = match read_texts(&fb.root) {
                Ok(texts) => texts,
                Err(fb_err) => {
                    eprintln!("[smelt-nsh] 自带插件 {id} 回退加载失败：{fb_err}");
                    stages[pending.slot_ix] = Some(SlotStage::Done {
                        info,
                        themes: fb_themes.map(|s| (id.clone(), s)),
                        texts: None,
                        install: None,
                        keep_script_id: None,
                    });
                    continue;
                }
            };

            if let Some(compiled) = try_load_precompiled(&fb, &sdk_fp) {
                self.compiled_roots.insert(id.clone(), fb.root.clone());
                stages[pending.slot_ix] = Some(SlotStage::Done {
                    info,
                    themes: fb_themes.map(|s| (id.clone(), s)),
                    texts: Some((id.clone(), fb_texts)),
                    install: Some(compiled),
                    keep_script_id: Some(id),
                });
            } else {
                fallback_jobs.push(CompileJob {
                    slot_ix: pending.slot_ix,
                    id,
                    root: fb.root.clone(),
                    entry: fb.manifest.entry,
                });
                stages[pending.slot_ix] = Some(SlotStage::WaitingFallbackCompile {
                    info,
                    fb_root: fb.root,
                    fb_themes,
                    fb_texts,
                });
            }
        }

        if !fallback_jobs.is_empty() {
            let stage_dir = self.plugins.stage();
            let plugins_stage = stage_dir.join("plugins");
            let compiled_results: Vec<Result<Compiled, String>> =
                if let Err(err) = fs::create_dir_all(&plugins_stage) {
                    let msg = format!("创建插件暂存目录 {} 失败：{err}", plugins_stage.display());
                    fallback_jobs.iter().map(|_| Err(msg.clone())).collect()
                } else {
                    let ui_sets = self.plugins.ui_sets();
                    run_parallel(&fallback_jobs, |job| {
                        let code = Plugins::compile_source_to_js(
                            stage_dir, ui_sets, &job.id, &job.root, &job.entry, false,
                        )?;
                        crate::plugins::validate_unresolved_references(&job.id, &code)?;
                        Ok(Compiled::from_source(job.id.clone(), code))
                    })
                };

            for (job, res) in fallback_jobs.into_iter().zip(compiled_results) {
                let Some(SlotStage::WaitingFallbackCompile {
                    info,
                    fb_root,
                    fb_themes,
                    fb_texts,
                }) = stages[job.slot_ix].take()
                else {
                    unreachable!();
                };
                let id = job.id;
                match res {
                    Ok(compiled) => {
                        self.compiled_roots.insert(id.clone(), fb_root);
                        stages[job.slot_ix] = Some(SlotStage::Done {
                            info,
                            themes: fb_themes.map(|s| (id.clone(), s)),
                            texts: Some((id.clone(), fb_texts)),
                            install: Some(compiled),
                            keep_script_id: Some(id),
                        });
                    }
                    Err(fb_err) => {
                        eprintln!("[smelt-nsh] 自带插件 {id} 回退编译失败：{fb_err}");
                        stages[job.slot_ix] = Some(SlotStage::Done {
                            info,
                            themes: fb_themes.map(|s| (id, s)),
                            texts: None,
                            install: None,
                            keep_script_id: None,
                        });
                    }
                }
            }
        }

        let mut loaded = Vec::with_capacity(stages.len());
        let mut themes = Vec::new();
        let mut texts_by_plugin = BTreeMap::new();
        let mut active_script_ids = BTreeSet::new();
        let mut install_changes = Vec::new();

        for stage in stages {
            let Some(SlotStage::Done {
                info,
                themes: plugin_themes,
                texts,
                install,
                keep_script_id,
            }) = stage
            else {
                unreachable!();
            };
            loaded.push(info);
            if let Some(theme_entry) = plugin_themes {
                themes.push(theme_entry);
            }
            if let Some((plugin_id, text_map)) = texts {
                texts_by_plugin.insert(plugin_id, text_map);
            }
            if let Some(script_id) = keep_script_id {
                active_script_ids.insert(script_id);
            }
            if let Some(compiled) = install {
                let change = self.plugins.insert_compiled(compiled);
                if !initial {
                    install_changes.push(change);
                }
            }
        }

        let mut changes = Vec::new();
        let mut removed_ids = Vec::new();
        let previously_installed: Vec<String> =
            self.plugins.installed().map(str::to_owned).collect();
        for id in previously_installed {
            if !active_script_ids.contains(&id) {
                changes.push(self.plugins.remove(&id));
                self.compiled_roots.remove(&id);
                self.failed_user_overrides.remove(&id);
                removed_ids.push(id);
            }
        }
        changes.extend(install_changes);

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
    inputs: mpsc::Sender<LiveInput>,
) -> notify::Result<Option<notify::RecommendedWatcher>> {
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
    if dirs.is_empty() {
        return Ok(None);
    }
    let matcher = dirs.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else {
            let _ = inputs.send(LiveInput::Changed(None));
            return;
        };
        if matches!(event.kind, notify::EventKind::Access(_)) {
            return;
        }
        if event.paths.is_empty() {
            let _ = inputs.send(LiveInput::Changed(None));
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
                    s.starts_with('.') || s == "node_modules" || s == "tsconfig.json"
                });
                if !ignored {
                    let _ = inputs.send(LiveInput::Changed(Some(dir.original.join(plugin_dir))));
                }
            }
        }
    })?;
    for dir in &dirs {
        watcher.watch(&dir.original, notify::RecursiveMode::Recursive)?;
    }
    Ok(Some(watcher))
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
        // 用户目录再放两份“beta”形成同来源重复，以及一份同 id 正常用户覆写稍后另测。
        write_plugin(&user.join("gamma"), "gamma", "main.tsx", beta_v1);
        write_plugin(&user.join("gamma-dup"), "gamma", "main.tsx", beta_v1);
        // 纯主题插件：只收主题 JSON，不起脚本 VM。
        write_plugin(
            &user.join("theme-pack"),
            "theme-pack",
            "themes/custom.json",
            r##"{"version":1,"name":"Custom","themes":[{"name":"Live Custom Dark","mode":"dark","colors":{"bg_base":"#121212","text_base":"#f5f5f5"}}]}"##,
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
            ShellSource::Source("export function Shell() { return null; }"),
            crate::ui_sets(),
            tx,
        );

        let initial = wait_update(&sync_rx, "首次扫描", |u| u.initial_scripts.is_some());
        assert_eq!(initial.changes.len(), 1);
        match &initial.changes[0] {
            Change::Start { plugins, .. } => {
                let ids: Vec<&str> = plugins.iter().map(|p| p.id()).collect();
                assert_eq!(ids, vec!["alpha", "beta", "gamma"]);
            }
            other => panic!("首次扫描应下发 Change::Start，收到 {other:?}"),
        }
        assert_eq!(initial.themes.len(), 1);
        assert_eq!(initial.themes[0].0, "theme-pack");
        assert!(
            initial
                .loaded
                .iter()
                .any(|p| p.id == "gamma" && p.root.ends_with("gamma-dup") && p.error.is_some()),
            "同来源重复 id 应记为重复错误"
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

        // 删除内置 alpha：下发 Remove("alpha")，beta 仍在。
        fs::remove_dir_all(builtin.join("alpha")).unwrap();
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

    #[test]
    fn user_override_shadows_builtin_and_automatically_falls_back_on_failure() {
        let root = TempDir::new("smelt-nsh-override-fallback");
        let builtin = root.0.join("builtin");
        let user = root.0.join("user");
        fs::create_dir_all(&builtin).unwrap();
        fs::create_dir_all(&user).unwrap();

        let builtin_code = r#"
import { Text } from "nsh";
export function Main() { return <Text>builtin-alpha</Text>; }
"#;
        let user_bad = r#"
import { Text } from "nsh";
export function Main() { return <Text>{brokenIdent}</Text>; }
"#;
        let user_good = r#"
import { Text } from "nsh";
export function Main() { return <Text>user-alpha</Text>; }
"#;

        write_plugin(&builtin.join("alpha"), "alpha", "main.tsx", builtin_code);
        write_plugin(&user.join("alpha"), "alpha", "main.tsx", user_bad);

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (sync_tx, sync_rx) = mpsc::channel();
        std::thread::spawn(move || {
            while let Some(update) = rx.blocking_recv() {
                if sync_tx.send(update).is_err() {
                    break;
                }
            }
        });

        let live = ShellLive::start(
            vec![
                SourceDir {
                    path: builtin.clone(),
                    source: PluginSource::Builtin,
                    watch: false,
                },
                SourceDir {
                    path: user.clone(),
                    source: PluginSource::User,
                    watch: true,
                },
            ],
            root.0.join("stage"),
            ShellSource::Source("export function Shell() { return null; }"),
            crate::ui_sets(),
            tx,
        );

        // 1. 首发时用户覆写含未声明标识符：应自动回退启动自带 builtin-alpha 并保留错误。
        let initial = wait_update(&sync_rx, "覆写首次回退", |u| {
            u.initial_scripts.is_some()
        });
        let alpha_info = initial.loaded.iter().find(|p| p.id == "alpha").unwrap();
        assert_eq!(alpha_info.source, PluginSource::User);
        assert!(alpha_info.overrides_builtin);
        assert!(alpha_info.fallback_to_builtin);
        assert!(alpha_info.error.is_some());
        match &initial.changes[0] {
            Change::Start { plugins, .. } => {
                let p = plugins.iter().find(|p| p.id() == "alpha").unwrap();
                let code = p.source_text().expect("期望源码");
                assert!(code.contains("builtin-alpha"), "{code}");
            }
            other => panic!("期望 Change::Start，收到 {other:?}"),
        }

        // 2. 修复用户覆写源码：热重载切回 user-alpha，清除错误。
        fs::write(user.join("alpha/main.tsx"), user_good).unwrap();
        let fixed = wait_update(&sync_rx, "修复用户覆写", |u| {
            u.loaded
                .iter()
                .any(|p| p.id == "alpha" && !p.fallback_to_builtin && p.error.is_none())
        });
        assert!(matches!(
            fixed.changes.as_slice(),
            [Change::Install(c)] if c.source_text().is_some_and(|s| s.contains("user-alpha"))
        ));

        // 3. 运行时崩溃请求回退：立即重装 builtin-alpha，状态标 fallback_to_builtin=true。
        live.request_builtin_fallback("alpha".into(), "TypeError: boom".into());
        let runtime_fb = wait_update(&sync_rx, "运行时回退", |u| {
            u.loaded.iter().any(|p| {
                p.id == "alpha"
                    && p.fallback_to_builtin
                    && p.error.as_deref() == Some("TypeError: boom")
            })
        });
        assert!(matches!(
            runtime_fb.changes.as_slice(),
            [Change::Install(c)] if c.source_text().is_some_and(|s| s.contains("builtin-alpha"))
        ));

        // 4. 删掉用户覆写目录：自动回到纯内置状态（无需重启 VM）。
        fs::remove_dir_all(user.join("alpha")).unwrap();
        let restored = wait_update(&sync_rx, "恢复内置状态", |u| {
            u.loaded.iter().any(|p| {
                p.id == "alpha"
                    && p.source == PluginSource::Builtin
                    && !p.overrides_builtin
                    && !p.fallback_to_builtin
                    && p.error.is_none()
            })
        });
        assert!(restored.changes.is_empty());
    }

    #[test]
    fn check_plugin_validates_id_path_traversal_themes_and_quickjs_imports() {
        let root = TempDir::new("smelt-nsh-check-cases");

        let bad_id = root.0.join("bad-id");
        write_plugin(
            &bad_id,
            "../escape",
            "themes/a.json",
            r#"{"name":"T","themes":[]}"#,
        );
        assert!(crate::check_plugin(&bad_id).is_err());

        let escape_entry = root.0.join("escape-entry");
        write_plugin(&escape_entry, "com.example.ok", "../escape.tsx", "");
        assert!(crate::check_plugin(&escape_entry).is_err());

        let bad_theme = root.0.join("bad-theme");
        write_plugin(&bad_theme, "com.example.theme", "themes/broken.json", "{");
        assert!(crate::check_plugin(&bad_theme).is_err());

        let reserved_theme = root.0.join("reserved-theme");
        write_plugin(
            &reserved_theme,
            "com.example.reserved",
            "themes/reserved.json",
            r##"{"version":1,"name":"Copy","themes":[{"name":"smelt Dark","mode":"dark","colors":{"bg_base":"#101010","text_base":"#fafafa"}}]}"##,
        );
        let reserved_err = crate::check_plugin(&reserved_theme).unwrap_err();
        assert!(reserved_err.contains("保留名"), "{reserved_err}");

        let valid_theme = root.0.join("valid-theme");
        write_plugin(
            &valid_theme,
            "com.example.custom-theme",
            "themes/custom.json",
            r##"{"version":1,"name":"Custom","themes":[{"name":"Custom Dark","mode":"dark","colors":{"bg_base":"#101010","text_base":"#fafafa"}}]}"##,
        );
        let theme_report = crate::check_plugin(&valid_theme).unwrap();
        assert!(theme_report.theme_only);
        assert!(theme_report.exports.is_empty());
        assert_eq!(theme_report.themes, vec!["Custom Dark"]);

        let bad_export = root.0.join("bad-export");
        write_plugin(
            &bad_export,
            "com.example.badexport",
            "nsh.tsx",
            "import { NoSuchSymbol } from \"nsh\";\nexport function Main() { return NoSuchSymbol; }\n",
        );
        let err = crate::check_plugin(&bad_export).unwrap_err();
        assert!(
            err.contains("QuickJS") || err.contains("NoSuchSymbol"),
            "{err}"
        );

        let valid = root.0.join("valid");
        write_plugin(
            &valid,
            "com.example.valid",
            "nsh.tsx",
            r#"
import { Text, View } from "nsh";
import { bootTask, initHost, registerViews, type ViewSpec } from "smelt/sdk";
bootTask(async () => {
  await initHost();
  const specs: ViewSpec[] = [{ name: "main", export: "Main", title: "Valid" }];
  await registerViews(specs);
});
export function Main() { return <View><Text>ok</Text></View>; }
"#,
        );
        let report = crate::check_plugin(&valid).unwrap();
        assert!(!report.theme_only);
        assert!(report.themes.is_empty());
        assert_eq!(report.exports, vec!["Main"]);
    }

    #[test]
    fn stage_builtin_plugins_produces_deterministic_bytecode_and_loads_without_sources() {
        let root = TempDir::new("smelt-nsh-stage-test");
        let src = root.0.join("src");
        let plugin_script = r#"
import { Text, View } from "nsh";
import { bootTask, initHost, registerViews } from "smelt/sdk";
bootTask(async () => {
  await initHost();
  await registerViews([{ name: "main", export: "Main", title: "Alpha" }]);
});
export function Main() { return <View><Text>alpha</Text></View>; }
"#;
        let alpha_src = src.join("alpha");
        write_plugin(&alpha_src, "com.example.alpha", "nsh.tsx", plugin_script);
        fs::create_dir_all(alpha_src.join("lib")).unwrap();
        fs::write(alpha_src.join("lib/util.ts"), "export const x = 1;").unwrap();
        fs::write(alpha_src.join("lib/helper.test.js"), "test()").unwrap();
        fs::create_dir_all(alpha_src.join("tests")).unwrap();
        fs::write(alpha_src.join("tests/unit.test.mjs"), "test()").unwrap();
        fs::create_dir_all(alpha_src.join(".git")).unwrap();
        fs::write(alpha_src.join(".git/HEAD"), "ref: refs/heads/main").unwrap();
        fs::create_dir_all(alpha_src.join("locales")).unwrap();
        fs::write(alpha_src.join("locales/en.json"), r#"{"hello":"Hello"}"#).unwrap();
        fs::create_dir_all(alpha_src.join("themes")).unwrap();
        fs::write(
            alpha_src.join("themes/a.json"),
            r##"{"version":1,"name":"AlphaTheme","themes":[{"name":"Alpha Custom","mode":"dark","colors":{"bg_base":"#101010","text_base":"#fafafa"}}]}"##,
        )
        .unwrap();

        write_plugin(
            &src.join("beta"),
            "com.example.beta",
            "main.tsx",
            "import { Text } from \"nsh\";\nexport function Main() { return <Text>beta</Text>; }\n",
        );
        write_plugin(
            &src.join("theme"),
            "com.example.theme",
            "themes/t.json",
            r##"{"version":1,"name":"OnlyTheme","themes":[{"name":"Only Custom","mode":"light","colors":{"bg_base":"#fafafa","text_base":"#101010"}}]}"##,
        );

        let staged_a = root.0.join("staged-a");
        let staged_b = root.0.join("staged-b");
        let report_a = stage_builtin_plugins(&src, &staged_a).expect("第一次 stage 成功");
        let report_b = stage_builtin_plugins(&src, &staged_b).expect("第二次 stage 成功");
        assert_eq!(report_a.total, 3);
        assert_eq!(report_a.script_plugins, 2);
        assert_eq!(report_a.theme_only, 1);
        assert_eq!(report_b.total, 3);

        assert!(staged_a.join("alpha/nsh.tsx").is_file());
        assert!(staged_a.join("alpha/lib/util.ts").is_file());
        assert!(!staged_a.join("alpha/lib/helper.test.js").exists());
        assert!(!staged_a.join("alpha/tests").exists());
        assert!(!staged_a.join("alpha/.git").exists());
        assert!(staged_a.join("beta/main.tsx").is_file());

        for name in ["alpha", "beta"] {
            assert!(
                staged_a
                    .join(name)
                    .join(PRECOMPILED_BYTECODE_FILE)
                    .is_file()
            );
            assert!(staged_a.join(name).join(PRECOMPILED_META_FILE).is_file());
            assert_eq!(
                fs::read(staged_a.join(name).join(PRECOMPILED_BYTECODE_FILE)).unwrap(),
                fs::read(staged_b.join(name).join(PRECOMPILED_BYTECODE_FILE)).unwrap(),
                "{name} 预编译字节码应完全确定性",
            );
            assert_eq!(
                fs::read(staged_a.join(name).join(PRECOMPILED_META_FILE)).unwrap(),
                fs::read(staged_b.join(name).join(PRECOMPILED_META_FILE)).unwrap(),
                "{name} 预编译元数据应完全确定性",
            );
        }
        assert!(staged_a.join("alpha/locales/en.json").is_file());
        assert!(staged_a.join("alpha/themes/a.json").is_file());
        assert!(staged_a.join("theme/themes/t.json").is_file());

        // 用户目录即使放了合法 meta + 损坏字节码，也必须忽略 .nshc 走源码编译；
        // 发版只读 staged_a 保留了 .tsx 仍应优先走预编译字节码；watch=false 的过期字节码但带源码的目录回退源码。
        let user_dir = root.0.join("user");
        let user_plugin = user_dir.join("u");
        write_plugin(
            &user_plugin,
            "com.example.user",
            "main.tsx",
            "import { Text } from \"nsh\";\nexport function Main() { return <Text>user</Text>; }\n",
        );
        fs::write(user_plugin.join(PRECOMPILED_BYTECODE_FILE), b"not-bytecode").unwrap();
        fs::write(
            user_plugin.join(PRECOMPILED_META_FILE),
            fs::read(staged_a.join("beta").join(PRECOMPILED_META_FILE)).unwrap(),
        )
        .unwrap();

        let fallback_dir = root.0.join("fallback");
        let stale_plugin = fallback_dir.join("stale");
        write_plugin(
            &stale_plugin,
            "com.example.stale",
            "main.tsx",
            "import { Text } from \"nsh\";\nexport function Main() { return <Text>stale</Text>; }\n",
        );
        fs::write(
            stale_plugin.join(PRECOMPILED_BYTECODE_FILE),
            b"not-bytecode",
        )
        .unwrap();
        let mut stale_meta: PrecompiledMeta = serde_json::from_slice(
            &fs::read(staged_a.join("beta").join(PRECOMPILED_META_FILE)).unwrap(),
        )
        .unwrap();
        stale_meta.sdk_fingerprint = "0000000000000000".to_owned();
        fs::write(
            stale_plugin.join(PRECOMPILED_META_FILE),
            serde_json::to_string(&stale_meta).unwrap(),
        )
        .unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (sync_tx, sync_rx) = mpsc::channel();
        std::thread::spawn(move || {
            while let Some(update) = rx.blocking_recv() {
                if sync_tx.send(update).is_err() {
                    break;
                }
            }
        });

        let stage_dir = root.0.join("stage");
        let _live = ShellLive::start(
            vec![
                SourceDir {
                    path: staged_a,
                    source: PluginSource::Builtin,
                    watch: false,
                },
                SourceDir {
                    path: fallback_dir,
                    source: PluginSource::Builtin,
                    watch: false,
                },
                SourceDir {
                    path: user_dir,
                    source: PluginSource::User,
                    watch: true,
                },
            ],
            stage_dir.clone(),
            ShellSource::Bytecode(crate::shell::MAIN_SHELL_BYTECODE),
            crate::ui_sets(),
            tx,
        );

        let initial = wait_update(&sync_rx, "预编译首发扫描", |u| {
            u.initial_scripts.is_some()
        });
        assert!(
            !stage_dir.join("shell").exists(),
            "预编译主壳不应落盘重打包"
        );
        assert!(
            initial.loaded.iter().all(|p| p.error.is_none()),
            "不应有加载错误：{:?}",
            initial
                .loaded
                .iter()
                .map(|p| (&p.id, &p.error))
                .collect::<Vec<_>>()
        );
        assert_eq!(initial.themes.len(), 2);
        match &initial.changes[0] {
            Change::Start { shell, plugins } => {
                assert!(matches!(shell, crate::plugins::CompiledCode::Bytecode(_)));
                let by_id: BTreeMap<&str, bool> =
                    plugins.iter().map(|p| (p.id(), p.is_bytecode())).collect();
                assert_eq!(by_id.get("com.example.alpha"), Some(&true));
                assert_eq!(by_id.get("com.example.beta"), Some(&true));
                assert_eq!(by_id.get("com.example.stale"), Some(&false));
                assert_eq!(by_id.get("com.example.user"), Some(&false));
            }
            other => panic!("期望 Change::Start，收到 {other:?}"),
        }
    }
}
