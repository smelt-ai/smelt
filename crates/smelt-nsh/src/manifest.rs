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

use crate::plugins::{
    BUILTINS_VM_ID, BuiltinCompileInput, Change, CompileMode, Compiled, ModulePackage, Plugins,
    is_ignored_plugin_entry, module_fingerprint, valid_module_specifier,
};

/// NSH 插件清单文件名。
pub const MANIFEST_FILE: &str = "smelt-plugin.json";
const BUILTINS_BYTECODE_FILE: &str = "builtins.nshc";
const BUILTINS_META_FILE: &str = "builtins.meta.json";

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct BuiltinPluginMeta {
    id: String,
    entry: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    module_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct BuiltinsPrecompiledMeta {
    bytecode_version: String,
    sdk_fingerprint: String,
    plugins: Vec<BuiltinPluginMeta>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    modules: BTreeMap<String, String>,
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
    pub module_plugins: usize,
    pub theme_only: usize,
    pub layout_only: usize,
    pub elapsed_ms: u64,
}

/// 纯声明式页面定义（从 `smelt-plugin.json` 的 `pages` 解析，无需启动脚本 VM）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclarativePageSpec {
    pub name: String,
    pub title_key: String,
    pub home: bool,
    pub index: Option<String>,
}

/// 纯声明式布局定义（从 `smelt-plugin.json` 的 `layouts` 解析，无需启动脚本 VM）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclarativeLayoutSpec {
    pub page: String,
    pub name: String,
    pub title_key: String,
    pub file: String,
    pub materials: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
pub(crate) struct DeclarativePage {
    pub name: String,
    pub title: smelt_plugin_api::Text,
    pub home: bool,
    pub index: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct DeclarativeLayout {
    pub page: String,
    pub name: String,
    pub title: smelt_plugin_api::Text,
    pub state: String,
    pub materials: smelt_plugin_api::LayoutMaterials,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct DeclarativePluginAssets {
    pub pages: Vec<DeclarativePage>,
    pub layouts: Vec<DeclarativeLayout>,
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
    pub module: Option<String>,
    pub types: Option<String>,
    pub pages: Vec<DeclarativePageSpec>,
    pub layouts: Vec<DeclarativeLayoutSpec>,
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
        let module = match obj.get("module") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(s)) => {
                if !valid_module_specifier(s) {
                    return Err(format!("{} 的 module 声明不合法：{s:?}", path.display()));
                }
                Some(s.to_owned())
            }
            Some(_) => return Err(format!("{} 的 module 必须是字符串", path.display())),
        };
        let types = match obj.get("types") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(s)) => {
                if s.trim().is_empty() || !s.ends_with(".d.ts") {
                    return Err(format!(
                        "{} 的 types 必须是插件目录内的 .d.ts 相对路径：{s:?}",
                        path.display()
                    ));
                }
                normalize_relative_entry(s)?;
                Some(s.to_owned())
            }
            Some(_) => return Err(format!("{} 的 types 必须是字符串", path.display())),
        };
        let pages = match obj.get("pages") {
            None | Some(serde_json::Value::Null) => Vec::new(),
            Some(serde_json::Value::Array(items)) => {
                let mut parsed = Vec::with_capacity(items.len());
                for item in items {
                    let page_obj = item
                        .as_object()
                        .ok_or_else(|| format!("{} 的 pages 项必须是对象", path.display()))?;
                    let page_name = page_obj
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .filter(|s| !s.trim().is_empty() && !s.contains('/'))
                        .ok_or_else(|| format!("{} 的 pages 项缺少合法 name", path.display()))?
                        .to_owned();
                    let title_key = page_obj
                        .get("title_key")
                        .and_then(serde_json::Value::as_str)
                        .filter(|s| !s.trim().is_empty())
                        .ok_or_else(|| {
                            format!("{} 的 pages[{page_name}] 缺少 title_key", path.display())
                        })?
                        .to_owned();
                    let home = page_obj
                        .get("home")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    let index = match page_obj.get("index") {
                        None | Some(serde_json::Value::Null) => None,
                        Some(serde_json::Value::String(s)) if !s.trim().is_empty() => {
                            Some(s.to_owned())
                        }
                        Some(_) => {
                            return Err(format!(
                                "{} 的 pages[{page_name}].index 必须是非空字符串",
                                path.display()
                            ));
                        }
                    };
                    parsed.push(DeclarativePageSpec {
                        name: page_name,
                        title_key,
                        home,
                        index,
                    });
                }
                parsed
            }
            Some(_) => return Err(format!("{} 的 pages 必须是数组", path.display())),
        };
        let layouts = match obj.get("layouts") {
            None | Some(serde_json::Value::Null) => Vec::new(),
            Some(serde_json::Value::Array(items)) => {
                let mut parsed = Vec::with_capacity(items.len());
                for item in items {
                    let layout_obj = item
                        .as_object()
                        .ok_or_else(|| format!("{} 的 layouts 项必须是对象", path.display()))?;
                    let page = layout_obj
                        .get("page")
                        .and_then(serde_json::Value::as_str)
                        .filter(|s| !s.trim().is_empty())
                        .ok_or_else(|| format!("{} 的 layouts 项缺少 page", path.display()))?
                        .to_owned();
                    let layout_name = layout_obj
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .filter(|s| !s.trim().is_empty() && !s.contains('/'))
                        .ok_or_else(|| format!("{} 的 layouts 项缺少合法 name", path.display()))?
                        .to_owned();
                    let title_key = layout_obj
                        .get("title_key")
                        .and_then(serde_json::Value::as_str)
                        .filter(|s| !s.trim().is_empty())
                        .ok_or_else(|| {
                            format!(
                                "{} 的 layouts[{layout_name}] 缺少 title_key",
                                path.display()
                            )
                        })?
                        .to_owned();
                    let file = layout_obj
                        .get("file")
                        .and_then(serde_json::Value::as_str)
                        .filter(|s| !s.trim().is_empty())
                        .ok_or_else(|| {
                            format!("{} 的 layouts[{layout_name}] 缺少 file", path.display())
                        })?
                        .to_owned();
                    normalize_relative_entry(&file)?;
                    let mut materials = BTreeMap::new();
                    if let Some(mats_val) = layout_obj.get("materials")
                        && !mats_val.is_null()
                    {
                        let mats_obj = mats_val.as_object().ok_or_else(|| {
                            format!(
                                "{} 的 layouts[{layout_name}].materials 必须是对象",
                                path.display()
                            )
                        })?;
                        for (side, mat_val) in mats_obj {
                            let mat = mat_val.as_str().ok_or_else(|| {
                                format!("{} 的 materials.{side} 必须是字符串", path.display())
                            })?;
                            if !matches!(side.as_str(), "left" | "right" | "bottom" | "center") {
                                return Err(format!(
                                    "{} 的 materials 存在未知区域 `{side}`",
                                    path.display()
                                ));
                            }
                            if smelt_plugin_api::Material::from_key(mat).is_none() {
                                return Err(format!(
                                    "{} 的 materials.{side} 不支持材质 `{mat}`",
                                    path.display()
                                ));
                            }
                            materials.insert(side.clone(), mat.to_owned());
                        }
                    }
                    parsed.push(DeclarativeLayoutSpec {
                        page,
                        name: layout_name,
                        title_key,
                        file,
                        materials,
                    });
                }
                parsed
            }
            Some(_) => return Err(format!("{} 的 layouts 必须是数组", path.display())),
        };
        let manifest = Self {
            id,
            name,
            version,
            entry,
            module,
            types,
            pages,
            layouts,
        };
        if !crate::plugins::valid_id(&manifest.id) {
            return Err(format!(
                "{} 的 id 不合法：{:?}",
                path.display(),
                manifest.id
            ));
        }
        let has_entry = !manifest.entry.trim().is_empty();
        let has_declarative_layouts = !manifest.pages.is_empty() || !manifest.layouts.is_empty();
        if !has_entry && !has_declarative_layouts {
            return Err(format!("{} 缺少 entry", path.display()));
        }
        if has_entry && has_declarative_layouts {
            return Err(format!(
                "{} 不能同时声明 entry 与 pages/layouts",
                path.display()
            ));
        }
        if manifest.is_module() && (manifest.is_theme_only() || manifest.is_layout_only()) {
            return Err(format!(
                "{} 共享模块插件不能同时是纯主题或纯布局插件",
                path.display()
            ));
        }
        Ok(manifest)
    }

    pub fn is_theme_only(&self) -> bool {
        Path::new(&self.entry)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
    }

    pub fn is_layout_only(&self) -> bool {
        self.entry.trim().is_empty() && (!self.pages.is_empty() || !self.layouts.is_empty())
    }

    pub fn is_module(&self) -> bool {
        self.module.is_some()
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
    pub layout_only: bool,
    pub is_module: bool,
    pub themes: Vec<String>,
    pub exports: Vec<String>,
}

fn module_package_from_parts(
    root: &Path,
    manifest: &PluginManifest,
    source: PluginSource,
) -> Result<Option<ModulePackage>, String> {
    let Some(spec) = manifest.module.as_ref() else {
        return Ok(None);
    };
    let normalized_entry = normalize_relative_entry(&manifest.entry)?;
    if !root.join(&normalized_entry).is_file() {
        return Err(format!("插件入口 {} 不存在", manifest.entry));
    }
    if let Some(types_rel) = manifest.types.as_deref() {
        let normalized_types = normalize_relative_entry(types_rel)?;
        if !root.join(normalized_types).is_file() {
            return Err(format!("类型声明文件 {types_rel} 不存在"));
        }
    }
    let fingerprint = module_fingerprint(root, spec, &manifest.entry)?;
    Ok(Some(ModulePackage {
        id: manifest.id.clone(),
        specifier: spec.clone(),
        root: root.to_path_buf(),
        entry: manifest.entry.clone(),
        types: manifest.types.clone(),
        fingerprint,
        source,
    }))
}

/// 从给定插件目录（含用户覆写优先级）收集全部有效的共享 JS 模块插件。
pub fn discover_module_packages(sources: &[SourceDir]) -> Vec<ModulePackage> {
    let slots = discover_slots(sources);
    let mut out = Vec::new();
    let mut seen_specifiers = BTreeSet::new();
    for slot in slots {
        let DiscoverySlot::Active { primary, fallback } = slot else {
            continue;
        };
        let chosen = if primary.manifest.is_module() {
            let spec_mismatch = fallback
                .as_ref()
                .and_then(|fb| fb.manifest.module.as_ref())
                .is_some_and(|fb_spec| primary.manifest.module.as_ref() != Some(fb_spec));
            if spec_mismatch {
                fallback.as_ref().and_then(|fb| {
                    module_package_from_parts(&fb.root, &fb.manifest, fb.source)
                        .ok()
                        .flatten()
                })
            } else {
                match module_package_from_parts(&primary.root, &primary.manifest, primary.source) {
                    Ok(Some(pkg)) => Some(pkg),
                    Ok(None) | Err(_) => fallback.as_ref().and_then(|fb| {
                        module_package_from_parts(&fb.root, &fb.manifest, fb.source)
                            .ok()
                            .flatten()
                    }),
                }
            }
        } else {
            fallback.as_ref().and_then(|fb| {
                module_package_from_parts(&fb.root, &fb.manifest, fb.source)
                    .ok()
                    .flatten()
            })
        };
        if let Some(pkg) = chosen
            && seen_specifiers.insert(pkg.specifier.clone())
        {
            out.push(pkg);
        }
    }
    out
}

fn resolve_localized_text(texts: &serde_json::Value, key: &str) -> smelt_plugin_api::Text {
    let entries: Vec<(gpui::SharedString, gpui::SharedString)> = texts
        .as_object()
        .map(|locales| {
            locales
                .iter()
                .filter_map(|(loc, table)| {
                    let value = table.get(key)?.as_str()?;
                    Some((
                        gpui::SharedString::from(loc.clone()),
                        gpui::SharedString::from(value.to_owned()),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    if entries.is_empty() {
        smelt_plugin_api::Text::from(key.to_owned())
    } else {
        smelt_plugin_api::Text::translations(entries)
    }
}

fn parse_layout_materials(
    materials: &BTreeMap<String, String>,
) -> Result<smelt_plugin_api::LayoutMaterials, String> {
    let mut out = smelt_plugin_api::LayoutMaterials::default();
    for (side, key) in materials {
        let Some(mat) = smelt_plugin_api::Material::from_key(key) else {
            return Err(format!("布局 materials.{side} 不支持材质 `{key}`"));
        };
        match side.as_str() {
            "left" => out.left = Some(mat),
            "right" => out.right = Some(mat),
            "bottom" => out.bottom = Some(mat),
            "center" => out.center = Some(mat),
            other => return Err(format!("布局 materials 存在未知区域 `{other}`")),
        }
    }
    Ok(out)
}

fn read_declarative_layouts(
    root: &Path,
    manifest: &PluginManifest,
    texts: &serde_json::Value,
) -> Result<DeclarativePluginAssets, String> {
    let mut pages = Vec::with_capacity(manifest.pages.len());
    for page in &manifest.pages {
        pages.push(DeclarativePage {
            name: page.name.clone(),
            title: resolve_localized_text(texts, &page.title_key),
            home: page.home,
            index: page.index.clone(),
        });
    }
    let mut layouts = Vec::with_capacity(manifest.layouts.len());
    for layout in &manifest.layouts {
        let rel = normalize_relative_entry(&layout.file)?;
        let path = root.join(&rel);
        let state = fs::read_to_string(&path)
            .map_err(|error| format!("读取布局 {} 失败：{error}", layout.file))?;
        smelt_plugin_api::parse_layout(&state)
            .map_err(|error| format!("校验布局 {} 失败：{error}", layout.file))?;
        let materials = parse_layout_materials(&layout.materials)?;
        layouts.push(DeclarativeLayout {
            page: layout.page.clone(),
            name: layout.name.clone(),
            title: resolve_localized_text(texts, &layout.title_key),
            state,
            materials,
        });
    }
    Ok(DeclarativePluginAssets { pages, layouts })
}

/// 检查插件目录是否合法（清单、locales、主题、声明式布局、共享模块声明、TSX 打包、import、未声明标识符与 QuickJS 字节码编译）。
pub fn check_plugin_with_sources(
    root: &Path,
    sources: &[SourceDir],
    ui_sets: Vec<SetDescription>,
) -> Result<PluginCheckReport, String> {
    let manifest = PluginManifest::read(root)?;
    let texts = read_texts(root)?;
    let themes = read_themes(root, &manifest.entry)?;
    let theme_names = validate_themes(&manifest.id, &themes)?;
    if manifest.is_layout_only() {
        read_declarative_layouts(root, &manifest, &texts)?;
        return Ok(PluginCheckReport {
            manifest,
            theme_only: false,
            layout_only: true,
            is_module: false,
            themes: theme_names,
            exports: Vec::new(),
        });
    }
    let normalized_entry = normalize_relative_entry(&manifest.entry)?;
    if !root.join(&normalized_entry).is_file() {
        return Err(format!("插件入口 {} 不存在", manifest.entry));
    }
    if let Some(types_rel) = manifest.types.as_deref() {
        let normalized_types = normalize_relative_entry(types_rel)?;
        if !root.join(normalized_types).is_file() {
            return Err(format!("类型声明文件 {types_rel} 不存在"));
        }
    }
    if manifest.is_theme_only() {
        return Ok(PluginCheckReport {
            manifest,
            theme_only: true,
            layout_only: false,
            is_module: false,
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
    let modules = discover_module_packages(sources);
    let is_module = manifest.is_module();
    let mode = match manifest.module.as_deref() {
        Some(spec) => CompileMode::Module {
            self_specifier: spec,
        },
        None => CompileMode::Plugin,
    };
    let compiled = Plugins::compile_source_to_js(
        &stage,
        &ui_sets,
        &modules,
        &manifest.id,
        root,
        &manifest.entry,
        mode,
        false,
    )?;
    crate::plugins::validate_unresolved_references(&manifest.id, &compiled.code)?;
    nsh_js::compile_bytecode_with(&compiled.code, nsh_js::Strip::Source, &ui_sets)
        .map_err(|error| format!("{} QuickJS 编译失败：{}", manifest.id, error.message))?;
    let exports = crate::plugins::extract_entry_exports(&root.join(&normalized_entry));
    Ok(PluginCheckReport {
        manifest,
        theme_only: false,
        layout_only: false,
        is_module,
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
        let kind = entry
            .file_type()
            .map_err(|e| format!("检查 {} 类型失败：{e}", entry.path().display()))?;
        if is_ignored_plugin_entry(&name_str, kind.is_dir()) {
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
/// - 共享模块插件校验打包与字节码编译并保留源码、`types`、`locales/`、`themes/`（不产 `.nshc`）；
/// - 全部应用脚本插件合并生成顶层单份 `builtins.nshc`（QuickJS 字节码，保留行号表）与 `builtins.meta.json`，
///   各子目录同时保留源码供 `smelt plugin fork` 或局部用户覆写编译使用；
/// - 纯主题与纯布局插件拷贝清单、文案与静态资产。
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
    let mut seen_modules = BTreeSet::new();
    let mut items = Vec::with_capacity(roots.len());
    for root in roots {
        let manifest = PluginManifest::read(&root)?;
        if !seen_ids.insert(manifest.id.clone()) {
            return Err(format!("内置插件目录存在重复 id：{}", manifest.id));
        }
        if let Some(spec) = manifest.module.as_ref()
            && !seen_modules.insert(spec.clone())
        {
            return Err(format!("内置插件目录存在重复模块声明：{spec}"));
        }
        let texts = read_texts(&root)?;
        if manifest.is_layout_only() {
            read_declarative_layouts(&root, &manifest, &texts)?;
        } else {
            let normalized_entry = normalize_relative_entry(&manifest.entry)?;
            if !root.join(&normalized_entry).is_file() {
                return Err(format!(
                    "插件 {} 入口 {} 不存在",
                    manifest.id, manifest.entry
                ));
            }
            if let Some(types_rel) = manifest.types.as_deref() {
                let normalized_types = normalize_relative_entry(types_rel)?;
                if !root.join(normalized_types).is_file() {
                    return Err(format!(
                        "插件 {} 类型声明 {} 不存在",
                        manifest.id, types_rel
                    ));
                }
            }
        }
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
    let mut layout_only_count = 0usize;
    let mut module_items: Vec<&StageItem> = Vec::new();
    let mut module_packages: Vec<ModulePackage> = Vec::new();
    let mut script_items: Vec<&StageItem> = Vec::new();
    for item in &items {
        let plugin_dst = staged_out.join(&item.dir_name);
        copy_dir_filtered(&item.root, &plugin_dst)?;

        if item.manifest.is_theme_only() {
            theme_only_count += 1;
        } else if item.manifest.is_layout_only() {
            layout_only_count += 1;
        } else if let Some(pkg) =
            module_package_from_parts(&item.root, &item.manifest, PluginSource::Builtin)?
        {
            module_items.push(item);
            module_packages.push(pkg);
        } else {
            script_items.push(item);
        }
    }

    if !module_items.is_empty() {
        for res in run_parallel(&module_items, |item| -> Result<(), String> {
            let id = &item.manifest.id;
            let spec = item.manifest.module.as_deref().unwrap_or_default();
            let compiled = Plugins::compile_source_to_js(
                &compile_stage,
                &ui_sets,
                &module_packages,
                id,
                &item.root,
                &item.manifest.entry,
                CompileMode::Module {
                    self_specifier: spec,
                },
                true,
            )?;
            crate::plugins::validate_unresolved_references(id, &compiled.code)?;
            nsh_js::compile_bytecode_with(&compiled.code, nsh_js::Strip::Source, &ui_sets)
                .map_err(|err| format!("{id} QuickJS 编译失败：{}", err.message))?;
            Ok(())
        }) {
            res?;
        }
    }

    script_items.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    if !script_items.is_empty() {
        let inputs: Vec<BuiltinCompileInput<'_>> = script_items
            .iter()
            .map(|item| BuiltinCompileInput {
                id: &item.manifest.id,
                root: &item.root,
                entry_rel: &item.manifest.entry,
            })
            .collect();
        let mut compiled = Plugins::compile_builtins_to_js(
            &compile_stage,
            &ui_sets,
            &module_packages,
            &inputs,
            true,
        )?;
        crate::plugins::validate_unresolved_references(BUILTINS_VM_ID, &compiled.code)?;
        let bytecode =
            nsh_js::compile_bytecode_with(&compiled.code, nsh_js::Strip::Source, &ui_sets)
                .map_err(|err| format!("{BUILTINS_VM_ID} QuickJS 编译失败：{}", err.message))?;
        fs::write(staged_out.join(BUILTINS_BYTECODE_FILE), &bytecode)
            .map_err(|e| format!("写入 {BUILTINS_BYTECODE_FILE} 失败：{e}"))?;

        let plugin_metas = script_items
            .iter()
            .map(|item| BuiltinPluginMeta {
                id: item.manifest.id.clone(),
                entry: item.manifest.entry.clone(),
                module_ids: compiled
                    .per_plugin_module_ids
                    .remove(&item.manifest.id)
                    .unwrap_or_default(),
            })
            .collect();
        let meta = BuiltinsPrecompiledMeta {
            bytecode_version: nsh_js::BYTECODE_VERSION.to_owned(),
            sdk_fingerprint: sdk_fp,
            plugins: plugin_metas,
            modules: compiled.used_modules,
        };
        let meta_json = serde_json::to_string_pretty(&meta)
            .map_err(|e| format!("序列化 {BUILTINS_META_FILE} 失败：{e}"))?;
        fs::write(staged_out.join(BUILTINS_META_FILE), meta_json)
            .map_err(|e| format!("写入 {BUILTINS_META_FILE} 失败：{e}"))?;
    }

    if dst_root.exists() {
        fs::remove_dir_all(dst_root)
            .map_err(|e| format!("清理旧目录 {} 失败：{e}", dst_root.display()))?;
    }
    fs::rename(&staged_out, dst_root)
        .map_err(|e| format!("落位目录 {} 失败：{e}", dst_root.display()))?;

    Ok(StageReport {
        total: items.len(),
        script_plugins: script_items.len(),
        module_plugins: module_items.len(),
        theme_only: theme_only_count,
        layout_only: layout_only_count,
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
    pub module: Option<String>,
    pub used_modules: Vec<String>,
    pub views: Vec<String>,
    pub error: Option<String>,
}

/// 一轮发现/热重载推回主线程的完整批次。
pub(crate) struct ShellUpdate {
    pub changes: Vec<Change>,
    pub removed_ids: Vec<String>,
    pub loaded: Vec<LoadedPluginInfo>,
    pub themes: Vec<(String, Vec<String>)>,
    pub declarative_layouts: Vec<(String, DeclarativePluginAssets)>,
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

struct DesiredBuiltin {
    slot_ix: usize,
    root: PathBuf,
    entry: String,
    source: PluginSource,
    watch: bool,
    texts: serde_json::Value,
    themes: Vec<String>,
    display_name: String,
    display_version: String,
    display_root: PathBuf,
    display_source: PluginSource,
    overrides_builtin: bool,
    fallback_to_builtin: bool,
    initial_error: Option<String>,
}

fn try_load_precompiled_builtins(
    desired: &BTreeMap<String, DesiredBuiltin>,
    sdk_fp: &str,
    active_modules: &BTreeMap<String, ModulePackage>,
) -> Option<(
    Compiled,
    BTreeMap<String, String>,
    BTreeMap<String, Vec<String>>,
)> {
    let first = desired.values().next()?;
    if desired
        .values()
        .any(|item| item.source != PluginSource::Builtin || item.watch)
    {
        return None;
    }
    let parent = first.root.parent()?;
    if desired
        .values()
        .any(|item| item.root.parent() != Some(parent))
    {
        return None;
    }
    let meta_path = parent.join(BUILTINS_META_FILE);
    let bytecode_path = parent.join(BUILTINS_BYTECODE_FILE);
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
    let meta: BuiltinsPrecompiledMeta = match serde_json::from_str(&meta_raw) {
        Ok(m) => m,
        Err(err) => {
            eprintln!(
                "[smelt-nsh] 解析 {} 失败，回退源码编译：{err}",
                meta_path.display()
            );
            return None;
        }
    };
    if meta.bytecode_version != nsh_js::BYTECODE_VERSION || meta.sdk_fingerprint != sdk_fp {
        eprintln!(
            "[smelt-nsh] 共享内置预编译元数据版本变化（version={} vs {}, sdk={} vs {}），回退源码编译",
            meta.bytecode_version,
            nsh_js::BYTECODE_VERSION,
            meta.sdk_fingerprint,
            sdk_fp,
        );
        return None;
    }
    if meta.plugins.len() != desired.len()
        || !meta
            .plugins
            .iter()
            .zip(desired.iter())
            .all(|(m, (id, item))| m.id == *id && m.entry == item.entry)
    {
        return None;
    }
    for (spec, expected_fp) in &meta.modules {
        let Some(active) = active_modules.get(spec) else {
            return None;
        };
        if active.fingerprint != *expected_fp {
            return None;
        }
    }
    match fs::read(&bytecode_path) {
        Ok(bytes) if !bytes.is_empty() => {
            let member_ids: Vec<String> = desired.keys().cloned().collect();
            let per_plugin_mod_ids: BTreeMap<String, Vec<String>> = meta
                .plugins
                .into_iter()
                .map(|m| (m.id, m.module_ids))
                .collect();
            Some((
                Compiled::from_bytecode_multi(BUILTINS_VM_ID.to_owned(), member_ids, bytes),
                meta.modules,
                per_plugin_mod_ids,
            ))
        }
        Ok(_) => {
            eprintln!("[smelt-nsh] {} 为空，回退源码编译", bytecode_path.display());
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
                        builtin_members: BTreeMap::new(),
                        builtin_module_deps: BTreeMap::new(),
                        standalone_roots: BTreeMap::new(),
                        standalone_module_deps: BTreeMap::new(),
                        standalone_module_ids: BTreeMap::new(),
                        cached_modules: BTreeMap::new(),
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

struct BuiltinMemberRecord {
    root: PathBuf,
    entry: String,
    used_module_ids: Vec<String>,
}

struct Worker {
    sources: Vec<SourceDir>,
    plugins: Plugins,
    updates: UnboundedSender<ShellUpdate>,
    /// 共享内置 VM（`__builtins__`）当前装入的成员记录。
    builtin_members: BTreeMap<String, BuiltinMemberRecord>,
    /// 共享内置 VM 上次加载命中的共享模块指纹表。
    builtin_module_deps: BTreeMap<String, String>,
    /// 当前以独立 VM 运行的插件 id → 目录路径（含用户插件与被拆出的用户模块消费方）。
    standalone_roots: BTreeMap<String, PathBuf>,
    /// 独立 VM 插件 id → 上次加载所依赖的共享模块指纹表。
    standalone_module_deps: BTreeMap<String, BTreeMap<String, String>>,
    /// 独立 VM 插件 id → 上次加载所依赖的共享模块插件 id 列表。
    standalone_module_ids: BTreeMap<String, Vec<String>>,
    /// 共享模块插件 id → (上次生效目录路径, 包描述)。
    cached_modules: BTreeMap<String, (PathBuf, ModulePackage)>,
    /// 编译/校验/启动失败且已回退到自带版本的用户覆写插件：`id -> (failed_user_root, error)`。
    failed_user_overrides: BTreeMap<String, (PathBuf, String)>,
}

struct CompileJob {
    slot_ix: usize,
    id: String,
    root: PathBuf,
    entry: String,
}

type CompileJobOutput = Result<(Compiled, BTreeMap<String, String>, Vec<String>), String>;

fn compile_jobs_parallel(
    plugins: &Plugins,
    modules: &[ModulePackage],
    jobs: &[CompileJob],
) -> Vec<CompileJobOutput> {
    if jobs.is_empty() {
        return Vec::new();
    }
    let stage_dir = plugins.stage();
    let plugins_stage = stage_dir.join("plugins");
    if let Err(err) = fs::create_dir_all(&plugins_stage) {
        let msg = format!("创建插件暂存目录 {} 失败：{err}", plugins_stage.display());
        return jobs.iter().map(|_| Err(msg.clone())).collect();
    }
    let ui_sets = plugins.ui_sets();
    run_parallel(jobs, |job| {
        let compiled = Plugins::compile_source_to_js(
            stage_dir,
            ui_sets,
            modules,
            &job.id,
            &job.root,
            &job.entry,
            CompileMode::Plugin,
            false,
        )?;
        crate::plugins::validate_unresolved_references(&job.id, &compiled.code)?;
        Ok((
            Compiled::from_source(job.id.clone(), compiled.code),
            compiled.used_modules,
            compiled.used_module_ids,
        ))
    })
}

fn validate_module_source(
    plugins: &Plugins,
    id: &str,
    root: &Path,
    entry: &str,
    specifier: &str,
) -> Result<(), String> {
    let stage_dir = plugins.stage();
    let plugins_stage = stage_dir.join("plugins");
    fs::create_dir_all(&plugins_stage)
        .map_err(|err| format!("创建插件暂存目录 {} 失败：{err}", plugins_stage.display()))?;
    let ui_sets = plugins.ui_sets();
    let compiled = Plugins::compile_source_to_js(
        stage_dir,
        ui_sets,
        &[],
        id,
        root,
        entry,
        CompileMode::Module {
            self_specifier: specifier,
        },
        false,
    )?;
    crate::plugins::validate_unresolved_references(id, &compiled.code)?;
    nsh_js::compile_bytecode_with(&compiled.code, nsh_js::Strip::Source, ui_sets)
        .map_err(|error| format!("{id} QuickJS 编译失败：{}", error.message))?;
    Ok(())
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

    fn resolve_override_failure(
        &mut self,
        id: &str,
        primary_root: &Path,
        fallback_present: bool,
        fallback_requests: &mut BTreeMap<String, String>,
    ) -> Option<String> {
        if let Some(reason) = fallback_requests.remove(id)
            && fallback_present
        {
            self.failed_user_overrides
                .insert(id.to_owned(), (primary_root.to_path_buf(), reason.clone()));
            Some(reason)
        } else if fallback_present {
            match self.failed_user_overrides.get(id) {
                Some((failed_root, prev_reason)) if failed_root == primary_root => {
                    Some(prev_reason.clone())
                }
                _ => {
                    self.failed_user_overrides.remove(id);
                    None
                }
            }
        } else {
            self.failed_user_overrides.remove(id);
            None
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
            self.cached_modules.clear();
        } else if let Some(changed) = changed_roots {
            self.failed_user_overrides
                .retain(|_, (failed_root, _)| !changed.contains(failed_root));
            self.cached_modules
                .retain(|_, (mod_root, _)| !changed.contains(mod_root));
        }

        let slots = discover_slots(&self.sources);
        let sdk_fp = crate::plugins::sdk_fingerprint();

        let previously_active_scripts: BTreeSet<String> = self
            .builtin_members
            .keys()
            .cloned()
            .chain(self.standalone_roots.keys().cloned())
            .collect();

        struct PendingFallback {
            slot_ix: usize,
            primary: Discovered,
            fallback: Discovered,
            reason: String,
        }

        struct BuiltinCandidate {
            slot_ix: usize,
            id: String,
            discovered: Discovered,
            display_name: String,
            display_version: String,
            display_root: PathBuf,
            display_source: PluginSource,
            overrides_builtin: bool,
            fallback_to_builtin: bool,
            error: Option<String>,
            themes: Vec<String>,
        }

        enum SlotStage {
            Done {
                info: LoadedPluginInfo,
                themes: Option<(String, Vec<String>)>,
                texts: Option<(String, serde_json::Value)>,
                install: Option<Compiled>,
                keep_standalone_id: Option<String>,
            },
            WaitingUserCompile {
                primary: Discovered,
                fallback: Option<Discovered>,
                themes: Vec<String>,
                texts: serde_json::Value,
                theme_error: Option<String>,
            },
            WaitingSplitCompile {
                cand: BuiltinCandidate,
                texts: serde_json::Value,
            },
        }

        let mut stages: Vec<Option<SlotStage>> = (0..slots.len()).map(|_| None).collect();
        let mut active_modules: BTreeMap<String, ModulePackage> = BTreeMap::new();
        let mut claimed_module_specs: BTreeMap<String, PathBuf> = BTreeMap::new();
        let mut seen_module_ids = BTreeSet::new();
        let mut app_slots: Vec<(usize, DiscoverySlot)> = Vec::new();

        // Phase 1：先解析、校验并确定全部共享 JS 模块插件的生效版本与指纹，
        // 若用户覆写模块出错且存在自带备份，则立刻回退到自带模块后再编消费方。
        for (slot_ix, slot) in slots.into_iter().enumerate() {
            let is_module_slot = matches!(
                &slot,
                DiscoverySlot::Active { primary, fallback }
                if primary.manifest.is_module()
                    || fallback.as_ref().is_some_and(|fb| fb.manifest.is_module())
            );
            if !is_module_slot {
                app_slots.push((slot_ix, slot));
                continue;
            }
            let DiscoverySlot::Active { primary, fallback } = slot else {
                unreachable!();
            };
            let id = primary.manifest.id.clone();
            seen_module_ids.insert(id.clone());
            let overrides_builtin = fallback.is_some();
            let forced_reason = self.resolve_override_failure(
                &id,
                &primary.root,
                overrides_builtin,
                &mut fallback_requests,
            );

            let spec_mismatch_err = fallback
                .as_ref()
                .and_then(|fb| fb.manifest.module.as_ref())
                .filter(|fb_spec| primary.manifest.module.as_ref() != Some(*fb_spec))
                .map(|fb_spec| {
                    format!("覆写内置模块插件 {id} 必须保留一致的 module 声明（期望 {fb_spec}）")
                });

            let failure_reason = forced_reason.or(spec_mismatch_err);

            if failure_reason.is_none() {
                let theme_res =
                    read_themes(&primary.root, &primary.manifest.entry).and_then(|sources| {
                        validate_themes(&id, &sources)?;
                        Ok(sources)
                    });
                let cached = (!full_rescan)
                    .then(|| self.cached_modules.get(&id))
                    .flatten()
                    .filter(|(root, _)| root == &primary.root)
                    .map(|(_, pkg)| pkg.clone());

                let (themes, validate_res) = match theme_res {
                    Err(err) => (Vec::new(), Err(err)),
                    Ok(themes) => {
                        if let Some(pkg) = cached {
                            (themes, Ok(pkg))
                        } else {
                            let res = read_texts(&primary.root)
                                .and_then(|_| {
                                    module_package_from_parts(
                                        &primary.root,
                                        &primary.manifest,
                                        primary.source,
                                    )
                                })
                                .and_then(|opt| {
                                    opt.ok_or_else(|| format!("模块插件 {id} 缺少 module 声明"))
                                })
                                .and_then(|pkg| {
                                    if primary.source == PluginSource::User || primary.watch {
                                        validate_module_source(
                                            &self.plugins,
                                            &id,
                                            &primary.root,
                                            &primary.manifest.entry,
                                            &pkg.specifier,
                                        )?;
                                    }
                                    Ok(pkg)
                                });
                            (themes, res)
                        }
                    }
                };

                match validate_res {
                    Ok(pkg) => {
                        if let Some(conflict_root) = claimed_module_specs.get(&pkg.specifier) {
                            let msg = format!(
                                "与 {} 的共享模块 `{}` 冲突，已跳过",
                                conflict_root.display(),
                                pkg.specifier
                            );
                            stages[slot_ix] = Some(SlotStage::Done {
                                info: LoadedPluginInfo {
                                    id,
                                    name: primary.manifest.name,
                                    version: primary.manifest.version,
                                    root: primary.root,
                                    source: primary.source,
                                    overrides_builtin,
                                    fallback_to_builtin: false,
                                    module: Some(pkg.specifier),
                                    used_modules: Vec::new(),
                                    views: Vec::new(),
                                    error: Some(msg),
                                },
                                themes: None,
                                texts: None,
                                install: None,
                                keep_standalone_id: None,
                            });
                        } else {
                            claimed_module_specs
                                .insert(pkg.specifier.clone(), primary.root.clone());
                            self.cached_modules
                                .insert(id.clone(), (primary.root.clone(), pkg.clone()));
                            active_modules.insert(pkg.specifier.clone(), pkg.clone());
                            stages[slot_ix] = Some(SlotStage::Done {
                                info: LoadedPluginInfo {
                                    id: id.clone(),
                                    name: primary.manifest.name,
                                    version: primary.manifest.version,
                                    root: primary.root,
                                    source: primary.source,
                                    overrides_builtin,
                                    fallback_to_builtin: false,
                                    module: Some(pkg.specifier),
                                    used_modules: Vec::new(),
                                    views: Vec::new(),
                                    error: None,
                                },
                                themes: (!themes.is_empty()).then_some((id, themes)),
                                texts: None,
                                install: None,
                                keep_standalone_id: None,
                            });
                        }
                        continue;
                    }
                    Err(err) => {
                        if let Some(fb) = fallback {
                            eprintln!(
                                "[smelt-nsh] 用户覆写模块插件 {id} 失败，已自动回退到自带版本：{err}"
                            );
                            self.failed_user_overrides
                                .insert(id.clone(), (primary.root.clone(), err.clone()));
                            self.cached_modules.remove(&id);
                            let fb_themes = read_themes(&fb.root, &fb.manifest.entry)
                                .and_then(|sources| {
                                    validate_themes(&id, &sources)?;
                                    Ok(sources)
                                })
                                .ok()
                                .filter(|sources| !sources.is_empty());
                            if let Ok(Some(fb_pkg)) =
                                module_package_from_parts(&fb.root, &fb.manifest, fb.source)
                            {
                                claimed_module_specs
                                    .insert(fb_pkg.specifier.clone(), fb.root.clone());
                                active_modules.insert(fb_pkg.specifier.clone(), fb_pkg.clone());
                            }
                            stages[slot_ix] = Some(SlotStage::Done {
                                info: LoadedPluginInfo {
                                    id: id.clone(),
                                    name: primary.manifest.name,
                                    version: primary.manifest.version,
                                    root: primary.root,
                                    source: PluginSource::User,
                                    overrides_builtin: true,
                                    fallback_to_builtin: true,
                                    module: fb.manifest.module.clone(),
                                    used_modules: Vec::new(),
                                    views: Vec::new(),
                                    error: Some(err),
                                },
                                themes: fb_themes.map(|s| (id, s)),
                                texts: None,
                                install: None,
                                keep_standalone_id: None,
                            });
                        } else {
                            self.cached_modules.remove(&id);
                            stages[slot_ix] = Some(SlotStage::Done {
                                info: LoadedPluginInfo {
                                    id: id.clone(),
                                    name: primary.manifest.name,
                                    version: primary.manifest.version,
                                    root: primary.root,
                                    source: primary.source,
                                    overrides_builtin: false,
                                    fallback_to_builtin: false,
                                    module: primary.manifest.module.clone(),
                                    used_modules: Vec::new(),
                                    views: Vec::new(),
                                    error: Some(err),
                                },
                                themes: (!themes.is_empty()).then_some((id, themes)),
                                texts: None,
                                install: None,
                                keep_standalone_id: None,
                            });
                        }
                        continue;
                    }
                }
            } else if let (Some(err), Some(fb)) = (failure_reason, fallback) {
                self.failed_user_overrides
                    .insert(id.clone(), (primary.root.clone(), err.clone()));
                self.cached_modules.remove(&id);
                let fb_themes = read_themes(&fb.root, &fb.manifest.entry)
                    .and_then(|sources| {
                        validate_themes(&id, &sources)?;
                        Ok(sources)
                    })
                    .ok()
                    .filter(|sources| !sources.is_empty());
                if let Ok(Some(fb_pkg)) =
                    module_package_from_parts(&fb.root, &fb.manifest, fb.source)
                {
                    claimed_module_specs.insert(fb_pkg.specifier.clone(), fb.root.clone());
                    active_modules.insert(fb_pkg.specifier.clone(), fb_pkg.clone());
                }
                stages[slot_ix] = Some(SlotStage::Done {
                    info: LoadedPluginInfo {
                        id: id.clone(),
                        name: primary.manifest.name,
                        version: primary.manifest.version,
                        root: primary.root,
                        source: PluginSource::User,
                        overrides_builtin: true,
                        fallback_to_builtin: true,
                        module: fb.manifest.module.clone(),
                        used_modules: Vec::new(),
                        views: Vec::new(),
                        error: Some(err),
                    },
                    themes: fb_themes.map(|s| (id, s)),
                    texts: None,
                    install: None,
                    keep_standalone_id: None,
                });
            }
        }

        self.cached_modules
            .retain(|id, _| seen_module_ids.contains(id));
        let active_module_list: Vec<ModulePackage> = active_modules.values().cloned().collect();
        self.plugins.set_modules(active_module_list.clone());
        let user_module_specs: BTreeSet<String> = active_modules
            .values()
            .filter(|pkg| pkg.source == PluginSource::User)
            .map(|pkg| pkg.specifier.clone())
            .collect();

        // Phase 2：先把非脚本插件与用户脚本插件落位：
        // - 用户脚本插件（含同名覆写内置）编为独立 VM；
        // - 未被覆写的内置插件与回退到自带的插件进入 `builtin_candidates`，后续再分流：
        //   引用了用户覆写共享模块的拆出为独立 VM，其余全部归入共享 `__builtins__` VM。
        let mut user_jobs: Vec<CompileJob> = Vec::new();
        let mut pending_fallbacks: Vec<PendingFallback> = Vec::new();
        let mut builtin_candidates: Vec<BuiltinCandidate> = Vec::new();
        let mut declarative_layouts: Vec<(String, DeclarativePluginAssets)> = Vec::new();

        for (slot_ix, slot) in app_slots {
            match slot {
                DiscoverySlot::Duplicate { item, duplicate_of } => {
                    stages[slot_ix] = Some(SlotStage::Done {
                        info: LoadedPluginInfo {
                            id: item.manifest.id,
                            name: item.manifest.name,
                            version: item.manifest.version,
                            root: item.root,
                            source: item.source,
                            overrides_builtin: false,
                            fallback_to_builtin: false,
                            module: item.manifest.module,
                            used_modules: Vec::new(),
                            views: Vec::new(),
                            error: Some(format!("与 {} 重复，已跳过", duplicate_of.display())),
                        },
                        themes: None,
                        texts: None,
                        install: None,
                        keep_standalone_id: None,
                    });
                }
                DiscoverySlot::Active { primary, fallback } => {
                    let id = primary.manifest.id.clone();
                    let overrides_builtin = fallback.is_some();
                    let forced_reason = self.resolve_override_failure(
                        &id,
                        &primary.root,
                        overrides_builtin,
                        &mut fallback_requests,
                    );

                    if let (Some(reason), Some(fb)) = (forced_reason, fallback.as_ref()) {
                        pending_fallbacks.push(PendingFallback {
                            slot_ix,
                            primary,
                            fallback: fb.clone(),
                            reason,
                        });
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
                        continue;
                    }

                    if primary.manifest.is_theme_only() {
                        stages[slot_ix] = Some(SlotStage::Done {
                            info: LoadedPluginInfo {
                                id: id.clone(),
                                name: primary.manifest.name,
                                version: primary.manifest.version,
                                root: primary.root,
                                source: primary.source,
                                overrides_builtin,
                                fallback_to_builtin: false,
                                module: None,
                                used_modules: Vec::new(),
                                views: Vec::new(),
                                error: theme_error,
                            },
                            themes: (!themes.is_empty()).then_some((id, themes)),
                            texts: None,
                            install: None,
                            keep_standalone_id: None,
                        });
                        continue;
                    }

                    if primary.manifest.is_layout_only() {
                        let layout_res = read_texts(&primary.root).and_then(|texts| {
                            read_declarative_layouts(&primary.root, &primary.manifest, &texts)
                        });
                        match layout_res {
                            Ok(assets) => {
                                declarative_layouts.push((id.clone(), assets));
                                stages[slot_ix] = Some(SlotStage::Done {
                                    info: LoadedPluginInfo {
                                        id: id.clone(),
                                        name: primary.manifest.name,
                                        version: primary.manifest.version,
                                        root: primary.root,
                                        source: primary.source,
                                        overrides_builtin,
                                        fallback_to_builtin: false,
                                        module: None,
                                        used_modules: Vec::new(),
                                        views: Vec::new(),
                                        error: theme_error,
                                    },
                                    themes: (!themes.is_empty()).then_some((id, themes)),
                                    texts: None,
                                    install: None,
                                    keep_standalone_id: None,
                                });
                            }
                            Err(error) => {
                                if let Some(fb) = fallback {
                                    eprintln!(
                                        "[smelt-nsh] 用户覆写布局插件 {id} 失败，已自动回退到自带版本：{error}"
                                    );
                                    pending_fallbacks.push(PendingFallback {
                                        slot_ix,
                                        primary,
                                        fallback: fb,
                                        reason: error,
                                    });
                                } else {
                                    eprintln!("[smelt-nsh] 布局插件 {id} 加载失败：{error}");
                                    stages[slot_ix] = Some(SlotStage::Done {
                                        info: LoadedPluginInfo {
                                            id: id.clone(),
                                            name: primary.manifest.name,
                                            version: primary.manifest.version,
                                            root: primary.root,
                                            source: primary.source,
                                            overrides_builtin: false,
                                            fallback_to_builtin: false,
                                            module: None,
                                            used_modules: Vec::new(),
                                            views: Vec::new(),
                                            error: theme_error.or(Some(error)),
                                        },
                                        themes: (!themes.is_empty()).then_some((id, themes)),
                                        texts: None,
                                        install: None,
                                        keep_standalone_id: None,
                                    });
                                }
                            }
                        }
                        continue;
                    }

                    if primary.source == PluginSource::Builtin {
                        let name = primary.manifest.name.clone();
                        let version = primary.manifest.version.clone();
                        let root = primary.root.clone();
                        builtin_candidates.push(BuiltinCandidate {
                            slot_ix,
                            id,
                            discovered: primary,
                            display_name: name,
                            display_version: version,
                            display_root: root,
                            display_source: PluginSource::Builtin,
                            overrides_builtin: false,
                            fallback_to_builtin: false,
                            error: theme_error,
                            themes,
                        });
                        continue;
                    }

                    let deps_match = self.standalone_module_deps.get(&id).is_some_and(|deps| {
                        deps.iter().all(|(spec, fp)| {
                            active_modules
                                .get(spec)
                                .is_some_and(|m| m.fingerprint == *fp)
                        })
                    });
                    let unchanged = !full_rescan
                        && deps_match
                        && changed_roots.is_some_and(|changed| {
                            !changed.contains(&primary.root)
                                && self
                                    .standalone_roots
                                    .get(&id)
                                    .is_some_and(|prev| prev == &primary.root)
                        });
                    if unchanged {
                        let used_modules = self
                            .standalone_module_ids
                            .get(&id)
                            .cloned()
                            .unwrap_or_default();
                        stages[slot_ix] = Some(SlotStage::Done {
                            info: LoadedPluginInfo {
                                id: id.clone(),
                                name: primary.manifest.name,
                                version: primary.manifest.version,
                                root: primary.root,
                                source: primary.source,
                                overrides_builtin,
                                fallback_to_builtin: false,
                                module: None,
                                used_modules,
                                views: Vec::new(),
                                error: theme_error,
                            },
                            themes: (!themes.is_empty()).then_some((id.clone(), themes)),
                            texts: None,
                            install: None,
                            keep_standalone_id: Some(id),
                        });
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
                            } else {
                                eprintln!("[smelt-nsh] 插件 {id} 编译失败：{error}");
                                let had_vm = self.standalone_roots.contains_key(&id);
                                let used_modules = self
                                    .standalone_module_ids
                                    .get(&id)
                                    .cloned()
                                    .unwrap_or_default();
                                stages[slot_ix] = Some(SlotStage::Done {
                                    info: LoadedPluginInfo {
                                        id: id.clone(),
                                        name: primary.manifest.name,
                                        version: primary.manifest.version,
                                        root: primary.root,
                                        source: primary.source,
                                        overrides_builtin: false,
                                        fallback_to_builtin: false,
                                        module: None,
                                        used_modules,
                                        views: Vec::new(),
                                        error: theme_error.or(Some(error)),
                                    },
                                    themes: (!themes.is_empty()).then_some((id.clone(), themes)),
                                    texts: None,
                                    install: None,
                                    keep_standalone_id: had_vm.then_some(id),
                                });
                            }
                            continue;
                        }
                    };

                    user_jobs.push(CompileJob {
                        slot_ix,
                        id,
                        root: primary.root.clone(),
                        entry: primary.manifest.entry.clone(),
                    });
                    stages[slot_ix] = Some(SlotStage::WaitingUserCompile {
                        primary,
                        fallback,
                        themes,
                        texts,
                        theme_error,
                    });
                }
            }
        }

        if !user_jobs.is_empty() {
            let compiled_results =
                compile_jobs_parallel(&self.plugins, &active_module_list, &user_jobs);

            for (job, res) in user_jobs.into_iter().zip(compiled_results) {
                let Some(SlotStage::WaitingUserCompile {
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
                    Ok((compiled, deps, mod_ids)) => {
                        self.standalone_roots
                            .insert(id.clone(), primary.root.clone());
                        self.standalone_module_deps.insert(id.clone(), deps);
                        self.standalone_module_ids
                            .insert(id.clone(), mod_ids.clone());
                        stages[job.slot_ix] = Some(SlotStage::Done {
                            info: LoadedPluginInfo {
                                id: id.clone(),
                                name: primary.manifest.name,
                                version: primary.manifest.version,
                                root: primary.root,
                                source: primary.source,
                                overrides_builtin: fallback.is_some(),
                                fallback_to_builtin: false,
                                module: None,
                                used_modules: mod_ids,
                                views: Vec::new(),
                                error: theme_error,
                            },
                            themes: (!themes.is_empty()).then_some((id.clone(), themes)),
                            texts: Some((id.clone(), texts)),
                            install: Some(compiled),
                            keep_standalone_id: Some(id),
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
                            let had_vm = self.standalone_roots.contains_key(&id);
                            let used_modules = self
                                .standalone_module_ids
                                .get(&id)
                                .cloned()
                                .unwrap_or_default();
                            stages[job.slot_ix] = Some(SlotStage::Done {
                                info: LoadedPluginInfo {
                                    id: id.clone(),
                                    name: primary.manifest.name,
                                    version: primary.manifest.version,
                                    root: primary.root,
                                    source: primary.source,
                                    overrides_builtin: false,
                                    fallback_to_builtin: false,
                                    module: None,
                                    used_modules,
                                    views: Vec::new(),
                                    error: theme_error.or(Some(error)),
                                },
                                themes: (!themes.is_empty()).then_some((id.clone(), themes)),
                                texts: None,
                                install: None,
                                keep_standalone_id: had_vm.then_some(id),
                            });
                        }
                    }
                }
            }
        }

        for pending in pending_fallbacks {
            let id = pending.primary.manifest.id.clone();
            self.failed_user_overrides.insert(
                id.clone(),
                (pending.primary.root.clone(), pending.reason.clone()),
            );
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
                    info: LoadedPluginInfo {
                        id: id.clone(),
                        name: pending.primary.manifest.name,
                        version: pending.primary.manifest.version,
                        root: pending.primary.root,
                        source: PluginSource::User,
                        overrides_builtin: true,
                        fallback_to_builtin: true,
                        module: None,
                        used_modules: Vec::new(),
                        views: Vec::new(),
                        error: Some(pending.reason),
                    },
                    themes: fb_themes.map(|s| (id, s)),
                    texts: None,
                    install: None,
                    keep_standalone_id: None,
                });
                continue;
            }

            if fb.manifest.is_layout_only() {
                match read_texts(&fb.root)
                    .and_then(|texts| read_declarative_layouts(&fb.root, &fb.manifest, &texts))
                {
                    Ok(assets) => {
                        declarative_layouts.push((id.clone(), assets));
                    }
                    Err(fb_err) => {
                        eprintln!("[smelt-nsh] 自带布局插件 {id} 回退加载失败：{fb_err}");
                    }
                }
                stages[pending.slot_ix] = Some(SlotStage::Done {
                    info: LoadedPluginInfo {
                        id: id.clone(),
                        name: pending.primary.manifest.name,
                        version: pending.primary.manifest.version,
                        root: pending.primary.root,
                        source: PluginSource::User,
                        overrides_builtin: true,
                        fallback_to_builtin: true,
                        module: None,
                        used_modules: Vec::new(),
                        views: Vec::new(),
                        error: Some(pending.reason),
                    },
                    themes: fb_themes.map(|s| (id, s)),
                    texts: None,
                    install: None,
                    keep_standalone_id: None,
                });
                continue;
            }

            builtin_candidates.push(BuiltinCandidate {
                slot_ix: pending.slot_ix,
                id,
                discovered: fb,
                display_name: pending.primary.manifest.name,
                display_version: pending.primary.manifest.version,
                display_root: pending.primary.root,
                display_source: PluginSource::User,
                overrides_builtin: true,
                fallback_to_builtin: true,
                error: Some(pending.reason),
                themes: fb_themes.unwrap_or_default(),
            });
        }

        // Phase 2b：内置脚本分流 —— 依赖用户覆写共享模块的拆出为独立 VM，其余归入 `__builtins__`。
        let mut split_jobs: Vec<CompileJob> = Vec::new();
        let mut desired_builtins: BTreeMap<String, DesiredBuiltin> = BTreeMap::new();

        for cand in builtin_candidates {
            let split_to_standalone = Plugins::plugin_uses_any_module(
                &cand.discovered.root,
                &active_module_list,
                &user_module_specs,
            );
            if split_to_standalone {
                let id = cand.id.clone();
                let deps_match = self.standalone_module_deps.get(&id).is_some_and(|deps| {
                    deps.iter().all(|(spec, fp)| {
                        active_modules
                            .get(spec)
                            .is_some_and(|m| m.fingerprint == *fp)
                    })
                });
                let unchanged = !full_rescan
                    && deps_match
                    && changed_roots.is_some_and(|changed| {
                        !changed.contains(&cand.discovered.root)
                            && self
                                .standalone_roots
                                .get(&id)
                                .is_some_and(|prev| prev == &cand.discovered.root)
                    });
                if unchanged {
                    let used_modules = self
                        .standalone_module_ids
                        .get(&id)
                        .cloned()
                        .unwrap_or_default();
                    stages[cand.slot_ix] = Some(SlotStage::Done {
                        info: LoadedPluginInfo {
                            id: cand.id,
                            name: cand.display_name,
                            version: cand.display_version,
                            root: cand.display_root,
                            source: cand.display_source,
                            overrides_builtin: cand.overrides_builtin,
                            fallback_to_builtin: cand.fallback_to_builtin,
                            module: None,
                            used_modules,
                            views: Vec::new(),
                            error: cand.error,
                        },
                        themes: (!cand.themes.is_empty()).then_some((id.clone(), cand.themes)),
                        texts: None,
                        install: None,
                        keep_standalone_id: Some(id),
                    });
                    continue;
                }

                match read_texts(&cand.discovered.root) {
                    Ok(texts) => {
                        let slot_ix = cand.slot_ix;
                        let job_id = cand.id.clone();
                        let root = cand.discovered.root.clone();
                        let entry = cand.discovered.manifest.entry.clone();
                        split_jobs.push(CompileJob {
                            slot_ix,
                            id: job_id,
                            root,
                            entry,
                        });
                        stages[slot_ix] = Some(SlotStage::WaitingSplitCompile { cand, texts });
                    }
                    Err(fb_err) => {
                        eprintln!("[smelt-nsh] 自带插件 {} 拆出加载失败：{fb_err}", cand.id);
                        let slot_ix = cand.slot_ix;
                        stages[slot_ix] = Some(SlotStage::Done {
                            info: LoadedPluginInfo {
                                id: cand.id.clone(),
                                name: cand.display_name,
                                version: cand.display_version,
                                root: cand.display_root,
                                source: cand.display_source,
                                overrides_builtin: cand.overrides_builtin,
                                fallback_to_builtin: cand.fallback_to_builtin,
                                module: None,
                                used_modules: Vec::new(),
                                views: Vec::new(),
                                error: cand.error.or(Some(fb_err)),
                            },
                            themes: (!cand.themes.is_empty()).then_some((cand.id, cand.themes)),
                            texts: None,
                            install: None,
                            keep_standalone_id: None,
                        });
                    }
                }
            } else {
                let texts = match read_texts(&cand.discovered.root) {
                    Ok(texts) => texts,
                    Err(err) => {
                        eprintln!("[smelt-nsh] 插件 {} 读取文案失败：{err}", cand.id);
                        let slot_ix = cand.slot_ix;
                        stages[slot_ix] = Some(SlotStage::Done {
                            info: LoadedPluginInfo {
                                id: cand.id.clone(),
                                name: cand.display_name,
                                version: cand.display_version,
                                root: cand.display_root,
                                source: cand.display_source,
                                overrides_builtin: cand.overrides_builtin,
                                fallback_to_builtin: cand.fallback_to_builtin,
                                module: None,
                                used_modules: Vec::new(),
                                views: Vec::new(),
                                error: cand.error.or(Some(err)),
                            },
                            themes: (!cand.themes.is_empty()).then_some((cand.id, cand.themes)),
                            texts: None,
                            install: None,
                            keep_standalone_id: None,
                        });
                        continue;
                    }
                };
                desired_builtins.insert(
                    cand.id,
                    DesiredBuiltin {
                        slot_ix: cand.slot_ix,
                        root: cand.discovered.root,
                        entry: cand.discovered.manifest.entry,
                        source: cand.discovered.source,
                        watch: cand.discovered.watch,
                        texts,
                        themes: cand.themes,
                        display_name: cand.display_name,
                        display_version: cand.display_version,
                        display_root: cand.display_root,
                        display_source: cand.display_source,
                        overrides_builtin: cand.overrides_builtin,
                        fallback_to_builtin: cand.fallback_to_builtin,
                        initial_error: cand.error,
                    },
                );
            }
        }

        if !split_jobs.is_empty() {
            let compiled_results =
                compile_jobs_parallel(&self.plugins, &active_module_list, &split_jobs);

            for (job, res) in split_jobs.into_iter().zip(compiled_results) {
                let Some(SlotStage::WaitingSplitCompile { cand, texts }) =
                    stages[job.slot_ix].take()
                else {
                    unreachable!();
                };
                let id = job.id;
                match res {
                    Ok((compiled, deps, mod_ids)) => {
                        self.standalone_roots
                            .insert(id.clone(), cand.discovered.root);
                        self.standalone_module_deps.insert(id.clone(), deps);
                        self.standalone_module_ids
                            .insert(id.clone(), mod_ids.clone());
                        stages[job.slot_ix] = Some(SlotStage::Done {
                            info: LoadedPluginInfo {
                                id: cand.id,
                                name: cand.display_name,
                                version: cand.display_version,
                                root: cand.display_root,
                                source: cand.display_source,
                                overrides_builtin: cand.overrides_builtin,
                                fallback_to_builtin: cand.fallback_to_builtin,
                                module: None,
                                used_modules: mod_ids,
                                views: Vec::new(),
                                error: cand.error,
                            },
                            themes: (!cand.themes.is_empty()).then_some((id.clone(), cand.themes)),
                            texts: Some((id.clone(), texts)),
                            install: Some(compiled),
                            keep_standalone_id: Some(id),
                        });
                    }
                    Err(fb_err) => {
                        eprintln!("[smelt-nsh] 自带插件 {id} 拆出编译失败：{fb_err}");
                        self.standalone_module_deps.remove(&id);
                        let had_vm = self.standalone_roots.contains_key(&id);
                        stages[job.slot_ix] = Some(SlotStage::Done {
                            info: LoadedPluginInfo {
                                id: cand.id,
                                name: cand.display_name,
                                version: cand.display_version,
                                root: cand.display_root,
                                source: cand.display_source,
                                overrides_builtin: cand.overrides_builtin,
                                fallback_to_builtin: cand.fallback_to_builtin,
                                module: None,
                                used_modules: Vec::new(),
                                views: Vec::new(),
                                error: cand.error.or(Some(fb_err)),
                            },
                            themes: (!cand.themes.is_empty()).then_some((id.clone(), cand.themes)),
                            texts: None,
                            install: None,
                            keep_standalone_id: had_vm.then_some(id),
                        });
                    }
                }
            }
        }

        // Phase 3：协调共享 `__builtins__` VM —— 仅当成员集合、各自目录内容或依赖模块指纹变化时重建。
        let prev_has_builtins = self.plugins.installed().any(|id| id == BUILTINS_VM_ID);
        let mut builtins_install: Option<Compiled> = None;
        let mut remove_builtins_vm = false;
        let mut builtin_texts: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        let mut builtin_themes: Vec<(String, Vec<String>)> = Vec::new();
        let active_builtin_ids: BTreeSet<String> = desired_builtins.keys().cloned().collect();

        if desired_builtins.is_empty() {
            self.builtin_members.clear();
            self.builtin_module_deps.clear();
            if prev_has_builtins {
                remove_builtins_vm = true;
            }
        } else {
            let same_members = self.builtin_members.len() == desired_builtins.len()
                && desired_builtins.iter().all(|(id, item)| {
                    self.builtin_members
                        .get(id)
                        .is_some_and(|prev| prev.root == item.root && prev.entry == item.entry)
                });
            let no_member_dir_changed = changed_roots.is_some_and(|changed| {
                desired_builtins
                    .values()
                    .all(|item| !changed.contains(&item.root))
            });
            let deps_match = self.builtin_module_deps.iter().all(|(spec, fp)| {
                active_modules
                    .get(spec)
                    .is_some_and(|m| m.fingerprint == *fp)
            });
            let builtins_unchanged = prev_has_builtins
                && !initial
                && !full_rescan
                && same_members
                && no_member_dir_changed
                && deps_match;

            if builtins_unchanged {
                for (id, item) in desired_builtins {
                    let used_modules = self
                        .builtin_members
                        .get(&id)
                        .map(|m| m.used_module_ids.clone())
                        .unwrap_or_default();
                    let slot_ix = item.slot_ix;
                    if !item.themes.is_empty() {
                        builtin_themes.push((id.clone(), item.themes));
                    }
                    stages[slot_ix] = Some(SlotStage::Done {
                        info: LoadedPluginInfo {
                            id,
                            name: item.display_name,
                            version: item.display_version,
                            root: item.display_root,
                            source: item.display_source,
                            overrides_builtin: item.overrides_builtin,
                            fallback_to_builtin: item.fallback_to_builtin,
                            module: None,
                            used_modules,
                            views: Vec::new(),
                            error: item.initial_error,
                        },
                        themes: None,
                        texts: None,
                        install: None,
                        keep_standalone_id: None,
                    });
                }
            } else {
                let mut precompiled_or_built =
                    try_load_precompiled_builtins(&desired_builtins, &sdk_fp, &active_modules)
                        .map(Ok);
                if precompiled_or_built.is_none() {
                    let stage_dir = self.plugins.stage();
                    let plugins_stage = stage_dir.join("plugins");
                    let prep_res = fs::create_dir_all(&plugins_stage).map_err(|err| {
                        format!("创建插件暂存目录 {} 失败：{err}", plugins_stage.display())
                    });
                    let ui_sets = self.plugins.ui_sets();
                    let inputs: Vec<BuiltinCompileInput<'_>> = desired_builtins
                        .iter()
                        .map(|(id, item)| BuiltinCompileInput {
                            id,
                            root: &item.root,
                            entry_rel: &item.entry,
                        })
                        .collect();
                    let member_ids: Vec<String> = desired_builtins.keys().cloned().collect();
                    precompiled_or_built = Some(prep_res.and_then(|()| {
                        let compiled = Plugins::compile_builtins_to_js(
                            stage_dir,
                            ui_sets,
                            &active_module_list,
                            &inputs,
                            false,
                        )?;
                        crate::plugins::validate_unresolved_references(
                            BUILTINS_VM_ID,
                            &compiled.code,
                        )?;
                        Ok((
                            Compiled::from_source_multi(
                                BUILTINS_VM_ID.to_owned(),
                                member_ids,
                                compiled.code,
                            ),
                            compiled.used_modules,
                            compiled.per_plugin_module_ids,
                        ))
                    }));
                }

                match precompiled_or_built.expect("builtins build result") {
                    Ok((compiled, deps, per_plugin_mod_ids)) => {
                        let mut next_members = BTreeMap::new();
                        for (id, item) in desired_builtins {
                            let used_modules =
                                per_plugin_mod_ids.get(&id).cloned().unwrap_or_default();
                            next_members.insert(
                                id.clone(),
                                BuiltinMemberRecord {
                                    root: item.root,
                                    entry: item.entry,
                                    used_module_ids: used_modules.clone(),
                                },
                            );
                            builtin_texts.insert(id.clone(), item.texts);
                            if !item.themes.is_empty() {
                                builtin_themes.push((id.clone(), item.themes));
                            }
                            stages[item.slot_ix] = Some(SlotStage::Done {
                                info: LoadedPluginInfo {
                                    id,
                                    name: item.display_name,
                                    version: item.display_version,
                                    root: item.display_root,
                                    source: item.display_source,
                                    overrides_builtin: item.overrides_builtin,
                                    fallback_to_builtin: item.fallback_to_builtin,
                                    module: None,
                                    used_modules,
                                    views: Vec::new(),
                                    error: item.initial_error,
                                },
                                themes: None,
                                texts: None,
                                install: None,
                                keep_standalone_id: None,
                            });
                        }
                        self.builtin_members = next_members;
                        self.builtin_module_deps = deps;
                        builtins_install = Some(compiled);
                    }
                    Err(bundle_err) => {
                        eprintln!("[smelt-nsh] 共享内置 VM 编译失败：{bundle_err}");
                        // 精准定位到底是哪几个内置成员出错，确保报错写在该插件名下且保留旧 VM 不卸。
                        let suspects: BTreeSet<String> = match changed_roots {
                            Some(changed) if !full_rescan && !initial => desired_builtins
                                .iter()
                                .filter(|(_, item)| changed.contains(&item.root))
                                .map(|(id, _)| id.clone())
                                .collect(),
                            _ => desired_builtins.keys().cloned().collect(),
                        };
                        let probe_ids: BTreeSet<String> = if suspects.is_empty() {
                            desired_builtins.keys().cloned().collect()
                        } else {
                            suspects
                        };
                        let stage_dir = self.plugins.stage();
                        let ui_sets = self.plugins.ui_sets();
                        let mut member_errors: BTreeMap<String, String> = BTreeMap::new();
                        for id in &probe_ids {
                            let Some(item) = desired_builtins.get(id) else {
                                continue;
                            };
                            let single = Plugins::compile_source_to_js(
                                stage_dir,
                                ui_sets,
                                &active_module_list,
                                id,
                                &item.root,
                                &item.entry,
                                CompileMode::Plugin,
                                false,
                            )
                            .and_then(|c| {
                                crate::plugins::validate_unresolved_references(id, &c.code)
                            });
                            if let Err(err) = single {
                                member_errors.insert(id.clone(), err);
                            }
                        }
                        if member_errors.is_empty() {
                            for id in probe_ids {
                                member_errors.insert(id, bundle_err.clone());
                            }
                        }
                        for (id, item) in desired_builtins {
                            let used_modules = self
                                .builtin_members
                                .get(&id)
                                .map(|m| m.used_module_ids.clone())
                                .unwrap_or_default();
                            if !item.themes.is_empty() {
                                builtin_themes.push((id.clone(), item.themes));
                            }
                            let err = item.initial_error.or_else(|| member_errors.remove(&id));
                            stages[item.slot_ix] = Some(SlotStage::Done {
                                info: LoadedPluginInfo {
                                    id,
                                    name: item.display_name,
                                    version: item.display_version,
                                    root: item.display_root,
                                    source: item.display_source,
                                    overrides_builtin: item.overrides_builtin,
                                    fallback_to_builtin: item.fallback_to_builtin,
                                    module: None,
                                    used_modules,
                                    views: Vec::new(),
                                    error: err,
                                },
                                themes: None,
                                texts: None,
                                install: None,
                                keep_standalone_id: None,
                            });
                        }
                    }
                }
            }
        }

        let mut loaded = Vec::with_capacity(stages.len());
        let mut themes = Vec::new();
        let mut texts_by_plugin = builtin_texts;
        themes.extend(builtin_themes);
        let mut active_standalone_ids = BTreeSet::new();
        let mut standalone_installs = Vec::new();

        for stage in stages {
            let Some(SlotStage::Done {
                info,
                themes: plugin_themes,
                texts,
                install,
                keep_standalone_id,
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
            if let Some(script_id) = keep_standalone_id {
                active_standalone_ids.insert(script_id);
            }
            if let Some(compiled) = install {
                standalone_installs.push(compiled);
            }
        }

        let mut changes = Vec::new();
        let previously_installed: Vec<String> =
            self.plugins.installed().map(str::to_owned).collect();
        for vm_id in previously_installed {
            if vm_id == BUILTINS_VM_ID {
                if remove_builtins_vm {
                    let change = self.plugins.remove(&vm_id);
                    if !initial {
                        changes.push(change);
                    }
                }
                continue;
            }
            if !active_standalone_ids.contains(&vm_id) {
                let change = self.plugins.remove(&vm_id);
                if !initial {
                    changes.push(change);
                }
                self.standalone_roots.remove(&vm_id);
                self.standalone_module_deps.remove(&vm_id);
                self.standalone_module_ids.remove(&vm_id);
                if !active_builtin_ids.contains(&vm_id) {
                    self.failed_user_overrides.remove(&vm_id);
                }
            }
        }

        if let Some(compiled) = builtins_install {
            let change = self.plugins.insert_compiled(compiled);
            if !initial {
                changes.push(change);
            }
        }
        for compiled in standalone_installs {
            let change = self.plugins.insert_compiled(compiled);
            if !initial {
                changes.push(change);
            }
        }

        let current_active_scripts: BTreeSet<String> = active_builtin_ids
            .union(&active_standalone_ids)
            .cloned()
            .collect();
        let removed_ids: Vec<String> = previously_active_scripts
            .difference(&current_active_scripts)
            .cloned()
            .collect();
        for id in &removed_ids {
            self.failed_user_overrides.remove(id);
        }

        let initial_scripts = if initial {
            changes = vec![self.plugins.start()];
            Some(current_active_scripts.into_iter().collect())
        } else {
            None
        };
        ShellUpdate {
            changes,
            removed_ids,
            loaded,
            themes,
            declarative_layouts,
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

    /// 覆盖 `ShellLive` 共享内置 VM 与独立用户 VM 热重载完整链路：
    /// 1. 首次扫描将自带 `alpha`、`beta` 合进 `__builtins__` VM，用户 `gamma` 单独起 VM，纯主题包不起 VM；
    /// 2. 改 `alpha` 只重装 `__builtins__`，不重编也不卸载独立用户插件 `gamma`；
    /// 3. `alpha` 写入语法错时保留旧 `__builtins__`（不下发 Remove/Install）并准确把错误记在 `alpha`；修复后恢复；
    /// 4. 删除 `alpha` 后 `__builtins__` 仅保留 `beta` 并下发 `removed_ids: ["alpha"]`。
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
                assert_eq!(ids, vec![BUILTINS_VM_ID, "gamma"]);
                let builtins = plugins.iter().find(|p| p.id() == BUILTINS_VM_ID).unwrap();
                assert_eq!(builtins.members(), ["alpha", "beta"]);
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

        // 改动内置 alpha：只重编 __builtins__，不触碰独立用户插件 gamma。
        let alpha_v2 = alpha_v1.replace("icons/plus.svg", "icons/x.svg");
        fs::write(builtin.join("alpha/main.tsx"), &alpha_v2).unwrap();
        let edited = wait_update(&sync_rx, "增量重编 alpha", |u| {
            u.changes
                .iter()
                .any(|c| matches!(c, Change::Install(p) if p.id() == BUILTINS_VM_ID))
        });
        assert_eq!(edited.changes.len(), 1, "只应重装 __builtins__，不动 gamma");
        assert!(edited.removed_ids.is_empty());

        // alpha 写入语法错误：不卸载旧 __builtins__，只把错误记在 alpha 名下。
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

        // 修复 alpha：恢复 __builtins__ 热重载。
        fs::write(builtin.join("alpha/main.tsx"), &alpha_v2).unwrap();
        let fixed = wait_update(&sync_rx, "修复 alpha", |u| {
            u.changes
                .iter()
                .any(|c| matches!(c, Change::Install(p) if p.id() == BUILTINS_VM_ID))
                && u.loaded.iter().any(|p| {
                    p.id == "alpha" && p.source == PluginSource::Builtin && p.error.is_none()
                })
        });
        assert_eq!(fixed.changes.len(), 1);

        // 删除内置 alpha：__builtins__ 收敛到仅含 beta，同时下发 removed_ids: ["alpha"]。
        fs::remove_dir_all(builtin.join("alpha")).unwrap();
        let removed = wait_update(&sync_rx, "删除 alpha", |u| {
            u.removed_ids.iter().any(|id| id == "alpha")
        });
        assert!(
            removed
                .changes
                .iter()
                .any(|c| matches!(c, Change::Install(p) if p.id() == BUILTINS_VM_ID && p.members() == ["beta"]))
        );
        assert!(
            removed
                .loaded
                .iter()
                .any(|p| p.id == "beta" && p.error.is_none()),
            "删除 alpha 后 beta 应保持正常"
        );
    }

    /// 插件比 App 新（用了这版 SDK 没有的导出）：报错是一句能看懂的话，不是打包器的代码框。
    #[test]
    fn a_plugin_newer_than_the_sdk_says_so_in_one_line() {
        let root = TempDir::new("smelt-nsh-newer-sdk");
        let builtin = root.0.join("builtin");
        let user = root.0.join("user");
        fs::create_dir_all(&builtin).unwrap();
        write_plugin(
            &user.join("gamma"),
            "gamma",
            "main.tsx",
            r#"
import { Text } from "nsh";
import { SmeltFromTheFuture, SmeltIcon } from "smelt/sdk";
export function Main() { return <Text>{String(SmeltFromTheFuture ?? SmeltIcon)}</Text>; }
"#,
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
                    path: builtin,
                    source: PluginSource::Builtin,
                    watch: false,
                },
                SourceDir {
                    path: user,
                    source: PluginSource::User,
                    watch: false,
                },
            ],
            root.0.join("stage"),
            ShellSource::Source("export function Shell() { return null; }"),
            crate::ui_sets(),
            tx,
        );
        let initial = wait_update(&sync_rx, "首发", |u| u.initial_scripts.is_some());
        let gamma = initial.loaded.iter().find(|p| p.id == "gamma").unwrap();
        assert_eq!(
            gamma.error.as_deref(),
            Some("插件用到了这版 Smelt 没有的 SDK 接口（SmeltFromTheFuture），需要更新 Smelt")
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
        let beta_code = r#"
import { Text } from "nsh";
export function Main() { return <Text>builtin-beta</Text>; }
"#;
        let user_bad = r#"
import { Text } from "nsh";
export function Main() { return <Text>{brokenIdent}</Text>; }
"#;
        let user_good = r#"
import { Text } from "nsh";
export function Main() { return <Text>user-alpha</Text>; }
"#;
        let user_good_v2 = r#"
import { Text } from "nsh";
export function Main() { return <Text>user-alpha-v2</Text>; }
"#;

        write_plugin(&builtin.join("alpha"), "alpha", "main.tsx", builtin_code);
        write_plugin(&builtin.join("beta"), "beta", "main.tsx", beta_code);
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

        // 1. 首发时用户覆写含未声明标识符：alpha 回退并留在 1 个共享 __builtins__ VM 中。
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
                assert_eq!(plugins.len(), 1);
                let p = &plugins[0];
                assert_eq!(p.id(), BUILTINS_VM_ID);
                assert_eq!(p.members(), ["alpha", "beta"]);
                let code = p.source_text().expect("期望源码");
                assert!(code.contains("builtin-alpha"), "{code}");
            }
            other => panic!("期望 Change::Start，收到 {other:?}"),
        }

        // 2. 修复用户覆写源码：alpha 剥离到独立用户 VM，__builtins__ 仅留 beta；且再次改 alpha 时不动 __builtins__。
        fs::write(user.join("alpha/main.tsx"), user_good).unwrap();
        let fixed = wait_update(&sync_rx, "修复用户覆写", |u| {
            u.loaded
                .iter()
                .any(|p| p.id == "alpha" && !p.fallback_to_builtin && p.error.is_none())
        });
        assert!(fixed.changes.iter().any(|c| matches!(
            c,
            Change::Install(p) if p.id() == BUILTINS_VM_ID && p.members() == ["beta"]
        )));
        assert!(fixed.changes.iter().any(|c| matches!(
            c,
            Change::Install(p) if p.id() == "alpha" && p.source_text().is_some_and(|s| s.contains("user-alpha"))
        )));

        fs::write(user.join("alpha/main.tsx"), user_good_v2).unwrap();
        let edited_override = wait_update(&sync_rx, "再改用户覆写不动 __builtins__", |u| {
            u.changes
                .iter()
                .any(|c| matches!(c, Change::Install(p) if p.id() == "alpha"))
        });
        assert!(matches!(
            edited_override.changes.as_slice(),
            [Change::Install(c)] if c.id() == "alpha" && c.source_text().is_some_and(|s| s.contains("user-alpha-v2"))
        ));

        // 3. 运行时崩溃请求回退：卸载独立 VM alpha，并将 builtin-alpha 收回 __builtins__（回到 1 个 VM）。
        live.request_builtin_fallback("alpha".into(), "TypeError: boom".into());
        let runtime_fb = wait_update(&sync_rx, "运行时回退", |u| {
            u.loaded.iter().any(|p| {
                p.id == "alpha"
                    && p.fallback_to_builtin
                    && p.error.as_deref() == Some("TypeError: boom")
            })
        });
        assert!(
            runtime_fb
                .changes
                .iter()
                .any(|c| matches!(c, Change::Remove(id) if id == "alpha"))
        );
        assert!(runtime_fb.changes.iter().any(|c| matches!(
            c,
            Change::Install(p)
                if p.id() == BUILTINS_VM_ID
                    && p.members() == ["alpha", "beta"]
                    && p.source_text().is_some_and(|s| s.contains("builtin-alpha"))
        )));

        // 4. 删掉用户覆写目录：自动回到纯内置状态（alpha 已在 __builtins__ 中，无需重启 VM）。
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

        assert!(staged_a.join(BUILTINS_BYTECODE_FILE).is_file());
        assert!(staged_a.join(BUILTINS_META_FILE).is_file());
        assert_eq!(
            fs::read(staged_a.join(BUILTINS_BYTECODE_FILE)).unwrap(),
            fs::read(staged_b.join(BUILTINS_BYTECODE_FILE)).unwrap(),
            "共享内置预编译字节码应完全确定性",
        );
        assert_eq!(
            fs::read(staged_a.join(BUILTINS_META_FILE)).unwrap(),
            fs::read(staged_b.join(BUILTINS_META_FILE)).unwrap(),
            "共享内置预编译元数据应完全确定性",
        );
        for name in ["alpha", "beta"] {
            assert!(!staged_a.join(name).join("bundle.nshc").exists());
            assert!(!staged_a.join(name).join("bundle.meta.json").exists());
        }
        assert!(staged_a.join("alpha/locales/en.json").is_file());
        assert!(staged_a.join("alpha/themes/a.json").is_file());
        assert!(staged_a.join("theme/themes/t.json").is_file());

        // 用户目录即使放了合法 meta + 损坏字节码，也必须忽略 .nshc 走源码编译；
        // 发版只读 staged_a 保留了 .tsx 仍应优先走共享内置 builtins.nshc 字节码。
        let user_dir = root.0.join("user");
        let user_plugin = user_dir.join("u");
        write_plugin(
            &user_plugin,
            "com.example.user",
            "main.tsx",
            "import { Text } from \"nsh\";\nexport function Main() { return <Text>user</Text>; }\n",
        );
        fs::write(user_plugin.join("bundle.nshc"), b"not-bytecode").unwrap();
        fs::write(user_plugin.join("bundle.meta.json"), "{}").unwrap();

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
                assert_eq!(plugins.len(), 2);
                let builtins = plugins.iter().find(|p| p.id() == BUILTINS_VM_ID).unwrap();
                assert!(builtins.is_bytecode());
                assert_eq!(
                    builtins.members(),
                    ["com.example.alpha", "com.example.beta"]
                );
                let user = plugins
                    .iter()
                    .find(|p| p.id() == "com.example.user")
                    .unwrap();
                assert!(!user.is_bytecode());
            }
            other => panic!("期望 Change::Start，收到 {other:?}"),
        }

        // sdk_fingerprint 过期的 builtins.meta.json（但保留源码）应安全回退源码编译，零错误启动。
        let mut stale_meta: BuiltinsPrecompiledMeta =
            serde_json::from_slice(&fs::read(staged_b.join(BUILTINS_META_FILE)).unwrap()).unwrap();
        stale_meta.sdk_fingerprint = "0000000000000000".to_owned();
        fs::write(
            staged_b.join(BUILTINS_META_FILE),
            serde_json::to_string(&stale_meta).unwrap(),
        )
        .unwrap();
        fs::write(staged_b.join(BUILTINS_BYTECODE_FILE), b"not-bytecode").unwrap();

        let (tx2, mut rx2) = tokio::sync::mpsc::unbounded_channel();
        let (sync_tx2, sync_rx2) = mpsc::channel();
        std::thread::spawn(move || {
            while let Some(update) = rx2.blocking_recv() {
                if sync_tx2.send(update).is_err() {
                    break;
                }
            }
        });
        let _live2 = ShellLive::start(
            vec![SourceDir {
                path: staged_b,
                source: PluginSource::Builtin,
                watch: false,
            }],
            root.0.join("stage-stale"),
            ShellSource::Bytecode(crate::shell::MAIN_SHELL_BYTECODE),
            crate::ui_sets(),
            tx2,
        );
        let fallback = wait_update(&sync_rx2, "过期元数据回退源码", |u| {
            u.initial_scripts.is_some()
        });
        assert!(fallback.loaded.iter().all(|p| p.error.is_none()));
        match &fallback.changes[0] {
            Change::Start { plugins, .. } => {
                assert_eq!(plugins.len(), 1);
                assert_eq!(plugins[0].id(), BUILTINS_VM_ID);
                assert!(!plugins[0].is_bytecode());
            }
            other => panic!("期望 Change::Start，收到 {other:?}"),
        }
    }

    #[test]
    fn shared_module_plugin_stages_hot_reloads_dependents_and_falls_back_on_error() {
        let root = TempDir::new("smelt-nsh-module-plugin");
        let src = root.0.join("src");
        let ui_src = src.join("ui");
        fs::create_dir_all(ui_src.join("tests")).unwrap();
        fs::write(
            ui_src.join(MANIFEST_FILE),
            r#"{"id":"demo.ui","name":"Demo UI","version":"1.0.0","module":"demo/ui","entry":"index.tsx","types":"index.d.ts"}"#,
        )
        .unwrap();
        fs::write(
            ui_src.join("index.tsx"),
            "import { Text } from \"nsh\";\nexport function Widget() { return <Text>builtin-ui</Text>; }\n",
        )
        .unwrap();
        fs::write(
            ui_src.join("index.d.ts"),
            "import type { JSX } from \"../nsh/types\";\nexport declare function Widget(): JSX.Element;\n",
        )
        .unwrap();
        fs::write(ui_src.join("tests/unit.test.mjs"), "test('skip')").unwrap();

        write_plugin(
            &src.join("app"),
            "demo.app",
            "nsh.tsx",
            "import { View } from \"nsh\";\nimport { Widget } from \"demo/ui\";\nexport function Main() { return <View><Widget /></View>; }\n",
        );
        write_plugin(
            &src.join("other"),
            "demo.other",
            "nsh.tsx",
            "import { Text } from \"nsh\";\nexport function Main() { return <Text>other</Text>; }\n",
        );

        let staged_a = root.0.join("staged-a");
        let staged_b = root.0.join("staged-b");
        let rep_a = stage_builtin_plugins(&src, &staged_a).expect("module stage A");
        let rep_b = stage_builtin_plugins(&src, &staged_b).expect("module stage B");
        assert_eq!(rep_a.total, 3);
        assert_eq!(rep_a.module_plugins, 1);
        assert_eq!(rep_a.script_plugins, 2);
        assert_eq!(rep_a.theme_only, 0);
        assert_eq!(rep_b.module_plugins, 1);

        assert!(staged_a.join("ui/index.tsx").is_file());
        assert!(staged_a.join("ui/index.d.ts").is_file());
        assert!(!staged_a.join("ui/tests").exists());
        assert!(staged_a.join(BUILTINS_BYTECODE_FILE).is_file());
        assert!(staged_a.join(BUILTINS_META_FILE).is_file());

        let meta: BuiltinsPrecompiledMeta =
            serde_json::from_slice(&fs::read(staged_a.join(BUILTINS_META_FILE)).unwrap()).unwrap();
        assert!(meta.modules.contains_key("demo/ui"));
        assert_eq!(
            meta.plugins
                .iter()
                .find(|p| p.id == "demo.app")
                .map(|p| p.module_ids.clone()),
            Some(vec!["demo.ui".to_owned()])
        );
        assert_eq!(
            meta.plugins
                .iter()
                .find(|p| p.id == "demo.other")
                .map(|p| p.module_ids.clone()),
            Some(Vec::new())
        );
        assert_eq!(
            fs::read(staged_a.join(BUILTINS_BYTECODE_FILE)).unwrap(),
            fs::read(staged_b.join(BUILTINS_BYTECODE_FILE)).unwrap(),
        );

        let user_dir = root.0.join("user");
        fs::create_dir_all(&user_dir).unwrap();

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
                    path: staged_a,
                    source: PluginSource::Builtin,
                    watch: false,
                },
                SourceDir {
                    path: user_dir.clone(),
                    source: PluginSource::User,
                    watch: true,
                },
            ],
            root.0.join("stage"),
            ShellSource::Bytecode(crate::shell::MAIN_SHELL_BYTECODE),
            crate::ui_sets(),
            tx,
        );

        let initial = wait_update(&sync_rx, "共享模块首次加载", |u| {
            u.initial_scripts.is_some()
        });
        assert!(initial.loaded.iter().all(|p| p.error.is_none()));
        assert_eq!(
            initial
                .loaded
                .iter()
                .find(|p| p.id == "demo.ui")
                .and_then(|p| p.module.as_deref()),
            Some("demo/ui")
        );
        assert_eq!(
            initial
                .loaded
                .iter()
                .find(|p| p.id == "demo.app")
                .map(|p| p.used_modules.clone()),
            Some(vec!["demo.ui".to_owned()])
        );
        match &initial.changes[0] {
            Change::Start { plugins, .. } => {
                assert_eq!(plugins.len(), 1, "首发应收敛成 1 个共享内置 VM");
                assert_eq!(plugins[0].id(), BUILTINS_VM_ID);
                assert!(plugins[0].is_bytecode(), "首发应命中 builtins.nshc");
                assert_eq!(plugins[0].members(), ["demo.app", "demo.other"]);
            }
            other => panic!("期望 Change::Start，收到 {other:?}"),
        }

        // 2. 用户目录放入覆写版 demo.ui：只把 demo.app 拆出为独立 VM，demo.other 留在 __builtins__。
        let user_ui = user_dir.join("demo.ui");
        fs::create_dir_all(&user_ui).unwrap();
        fs::write(
            user_ui.join(MANIFEST_FILE),
            r#"{"id":"demo.ui","name":"Demo UI","version":"1.0.0","module":"demo/ui","entry":"index.tsx","types":"index.d.ts"}"#,
        )
        .unwrap();
        fs::write(
            user_ui.join("index.d.ts"),
            "import type { JSX } from \"../nsh/types\";\nexport declare function Widget(): JSX.Element;\n",
        )
        .unwrap();
        fs::write(
            user_ui.join("index.tsx"),
            "import { Text } from \"nsh\";\nexport function Widget() { return <Text>user-ui</Text>; }\n",
        )
        .unwrap();

        let overridden = wait_update(&sync_rx, "用户覆写模块联动拆出 demo.app", |u| {
            u.changes
                .iter()
                .any(|c| matches!(c, Change::Install(p) if p.id() == "demo.app"))
                && u.loaded
                    .iter()
                    .any(|p| p.id == "demo.ui" && p.overrides_builtin && !p.fallback_to_builtin)
        });
        assert_eq!(overridden.changes.len(), 2);
        assert!(overridden.changes.iter().any(|c| matches!(
            c,
            Change::Install(p) if p.id() == BUILTINS_VM_ID && p.members() == ["demo.other"]
        )));
        assert!(overridden.changes.iter().any(|c| matches!(
            c,
            Change::Install(c) if c.id() == "demo.app" && !c.is_bytecode() && c.source_text().is_some_and(|s| s.contains("user-ui"))
        )));

        // 再改一次 user demo.ui：__builtins__ 不重编、只增量重编已拆出的 demo.app。
        fs::write(
            user_ui.join("index.tsx"),
            "import { Text } from \"nsh\";\nexport function Widget() { return <Text>user-ui-v2</Text>; }\n",
        )
        .unwrap();
        let edited_mod = wait_update(&sync_rx, "再改用户模块不动 __builtins__", |u| {
            u.changes.iter().any(
                |c| matches!(c, Change::Install(p) if p.id() == "demo.app" && p.source_text().is_some_and(|s| s.contains("user-ui-v2"))),
            )
        });
        assert_eq!(edited_mod.changes.len(), 1);

        // 3. 把用户覆写模块改坏：自动回退自带 demo.ui，卸载独立 VM demo.app 并收回 __builtins__ 命中字节码！
        fs::write(
            user_ui.join("index.tsx"),
            "import { Text } from \"nsh\";\nexport function Widget() { return <Text>{brokenIdent}</Text>; }\n",
        )
        .unwrap();
        let fallback = wait_update(&sync_rx, "共享模块自动回退", |u| {
            u.loaded
                .iter()
                .any(|p| p.id == "demo.ui" && p.fallback_to_builtin && p.error.is_some())
                && u.changes
                    .iter()
                    .any(|c| matches!(c, Change::Install(p) if p.id() == BUILTINS_VM_ID))
        });
        assert!(
            fallback
                .changes
                .iter()
                .any(|c| matches!(c, Change::Remove(id) if id == "demo.app"))
        );
        assert!(fallback.changes.iter().any(|c| matches!(
            c,
            Change::Install(c) if c.id() == BUILTINS_VM_ID && c.is_bytecode() && c.members() == ["demo.app", "demo.other"]
        )));

        // 4. 删掉用户覆写模块：清除错误、恢复纯内置状态。
        fs::remove_dir_all(&user_ui).unwrap();
        let restored = wait_update(&sync_rx, "删掉用户模块恢复默认", |u| {
            u.loaded.iter().any(|p| {
                p.id == "demo.ui"
                    && p.source == PluginSource::Builtin
                    && !p.overrides_builtin
                    && !p.fallback_to_builtin
                    && p.error.is_none()
            })
        });
        assert!(restored.changes.is_empty());
    }

    #[test]
    fn layout_only_plugin_stages_without_bytecode_and_falls_back_on_error() {
        let root = TempDir::new("smelt-nsh-layout-plugin");
        let src = root.0.join("src");
        let layouts_src = src.join("layouts");
        fs::create_dir_all(layouts_src.join("layouts")).unwrap();
        fs::create_dir_all(layouts_src.join("locales")).unwrap();
        fs::create_dir_all(layouts_src.join("tests")).unwrap();
        fs::write(
            layouts_src.join(MANIFEST_FILE),
            r#"{
              "id": "demo.layouts",
              "name": "Layouts",
              "version": "1.0.0",
              "pages": [{ "name": "workbench", "title_key": "page.wb", "home": true }],
              "layouts": [{
                "page": "demo.layouts/workbench",
                "name": "std",
                "title_key": "layout.std",
                "file": "layouts/std.json",
                "materials": { "left": "sidebar" }
              }]
            }"#,
        )
        .unwrap();
        fs::write(
            layouts_src.join("locales/ui.yml"),
            "_version: 2\npage.wb:\n  en: Workbench\nlayout.std:\n  en: Standard\n",
        )
        .unwrap();
        fs::write(
            layouts_src.join("layouts/std.json"),
            r#"{"version":1,"center":{"panel_name":"TabPanel","info":{"tabs":{"active_index":0}},"children":[{"panel_name":"smelt/outlet","children":[],"info":{"panel":null}}]}}"#,
        )
        .unwrap();
        fs::write(layouts_src.join("tests/standard.test.mjs"), "test()").unwrap();

        let checked = crate::check_plugin(&layouts_src).unwrap();
        assert!(checked.layout_only);
        assert!(checked.exports.is_empty());

        let staged = root.0.join("staged");
        let report = stage_builtin_plugins(&src, &staged).unwrap();
        assert_eq!(report.total, 1);
        assert_eq!(report.script_plugins, 0);
        assert_eq!(report.layout_only, 1);
        assert!(!staged.join("layouts/bundle.nshc").exists());
        assert!(!staged.join("layouts/bundle.meta.json").exists());
        assert!(!staged.join("layouts/tests").exists());

        let user_dir = root.0.join("user");
        fs::create_dir_all(&user_dir).unwrap();
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
                    path: staged,
                    source: PluginSource::Builtin,
                    watch: false,
                },
                SourceDir {
                    path: user_dir.clone(),
                    source: PluginSource::User,
                    watch: true,
                },
            ],
            root.0.join("stage"),
            ShellSource::Bytecode(crate::shell::MAIN_SHELL_BYTECODE),
            crate::ui_sets(),
            tx,
        );

        let initial = wait_update(&sync_rx, "声明式布局首发", |u| {
            u.initial_scripts.is_some()
        });
        assert_eq!(initial.initial_scripts.as_deref(), Some(&[][..]));
        assert_eq!(initial.declarative_layouts.len(), 1);
        assert_eq!(initial.declarative_layouts[0].0, "demo.layouts");
        assert_eq!(initial.declarative_layouts[0].1.pages.len(), 1);
        assert_eq!(initial.declarative_layouts[0].1.layouts.len(), 1);
        assert!(initial.loaded[0].error.is_none());

        // 用户覆写坏 JSON：自动回退到内置声明式布局，并在 status 里保留错误。
        let user_layouts = user_dir.join("demo.layouts");
        fs::create_dir_all(user_layouts.join("layouts")).unwrap();
        fs::write(
            user_layouts.join(MANIFEST_FILE),
            r#"{
              "id": "demo.layouts",
              "name": "Layouts",
              "version": "1.0.0",
              "pages": [{ "name": "workbench", "title_key": "page.wb", "home": true }],
              "layouts": [{
                "page": "demo.layouts/workbench",
                "name": "std",
                "title_key": "layout.std",
                "file": "layouts/std.json"
              }]
            }"#,
        )
        .unwrap();
        fs::write(user_layouts.join("layouts/std.json"), "{broken json").unwrap();

        let fallback = wait_update(&sync_rx, "布局覆写损坏回退", |u| {
            u.loaded
                .iter()
                .any(|p| p.id == "demo.layouts" && p.fallback_to_builtin && p.error.is_some())
        });
        assert_eq!(fallback.declarative_layouts.len(), 1);
        assert_eq!(fallback.declarative_layouts[0].0, "demo.layouts");
    }
}
