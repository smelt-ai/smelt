//! 运行时装插件（A2 架构）：单引擎线程 + 单界面树（`Core`）+ 每插件独立 QuickJS VM。
//!
//! 外壳打成一包（`App.tsx` + `slots.tsx` + `Shell.tsx`），每个插件各自打成标准 ES 模块（[`Compiled`]），
//! 由 [`PluginHost`] 经 `AppView::load_plugin` 载入各自隔离的 QuickJS VM（`harden: true`、独立堆与
//! CPU 熔断、独立 `HostChannel`、独立 `TenantId`）：
//! - 装、改、删一个插件只重建它自己的 VM，外壳里的终端、对话和别的插件不动；
//! - 插件死循环、OOM、超节点配额或越权操作节点只停掉并清理它自己的租户，引擎与外壳不重启；
//! - 只有宿主外壳经内部插槽协议挂载顶层插件视图（`<Slot>` → `smelt.slot.ops` → 引擎内 [`PluginHost`]
//!   路由 → `smelt.slot.sync` → `mountSlot`），热重载时宿主按 `instances` 表原位重放；插件自身不能
//!   使用 `smelt/slots` 嵌套其它插件，复用的重画布一律走原子原生能力组件。
//!
//! 打包和读文件都阻塞，调用方放到后台做；[`PluginHost::apply`] 在主线程。

use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use nsh_bundler::BundleOptions;
use nsh_core::{access::Capabilities, element::description::SetDescription};
use nsh_engine::{AppError, AppErrorKind, HostCall, HostChannel, Limits, PluginConfig, Policy};
use nsh_platform_gpui::{App, AppView, Bundle};
use oxc::{
    allocator::Allocator, ast::ast::Statement, parser::Parser, semantic::SemanticBuilder,
    span::SourceType,
};
use serde_json::{Value, json};

const APP: &str = include_str!("plugins/App.tsx");
const PROVIDER: &str = include_str!("plugins/provider.tsx");
const SLOTS: &str = include_str!("plugins/slots.tsx");
const SDK: &str = include_str!("plugins/sdk.tsx");
const SHELL_NSH_CONFIG: &str = include_str!("plugins/shell.nsh.json");
const PLUGIN_NSH_CONFIG: &str = include_str!("plugins/plugin.nsh.json");

/// 外壳上报跨 VM 顶层视图插槽增改删批次。
const SLOT_OPS: &str = "smelt.slot.ops";
/// 宿主推给提供方插件的插槽同步批次（mount / update / unmount）。
const SLOT_SYNC: &str = "smelt.slot.sync";
/// 宿主推给外壳的插件故障状态（显示/清除兜底提示）。
const SLOT_FAILED: &str = "smelt.slot.failed";
/// 提供方插件挂好 `smelt.slot.sync` 监听后上报就绪，触发活跃实例重放。
const PLUGIN_READY: &str = "smelt.plugin.ready";
/// 外壳在宿主桥里的身份标识。
pub(crate) const SHELL_OWNER: &str = "__shell__";
/// 所有未被用户覆写、未依赖用户模块的内置脚本插件共驻的共享 VM 身份标识。
pub(crate) const BUILTINS_VM_ID: &str = "__builtins__";

/// 一个插件的入口文件，导出它的视图。
pub(crate) const ENTRY: &str = "main.tsx";

/// 编译结果：源码形式（用户插件 / 调试 / 热重载）或 QuickJS 字节码（预编译主壳与自带插件）。
#[derive(Clone, Debug)]
pub enum CompiledCode {
    Source(Arc<str>),
    Bytecode(Arc<[u8]>),
}

/// 一个 VM 承载的已编译可执行模块（单插件时 `members` 为 `[id]`，共享内置 VM 时为全部内置成员 id）。
#[derive(Clone, Debug)]
pub struct Compiled {
    id: String,
    members: Vec<String>,
    code: CompiledCode,
}

impl Compiled {
    pub(crate) fn from_source(id: String, source: String) -> Self {
        Self::from_source_multi(id.clone(), vec![id], source)
    }

    pub(crate) fn from_bytecode_multi(id: String, members: Vec<String>, bytes: Vec<u8>) -> Self {
        Self {
            id,
            members,
            code: CompiledCode::Bytecode(Arc::from(bytes)),
        }
    }

    pub(crate) fn from_source_multi(id: String, members: Vec<String>, source: String) -> Self {
        Self {
            id,
            members,
            code: CompiledCode::Source(source.into()),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn members(&self) -> &[String] {
        &self.members
    }

    #[cfg(test)]
    pub(crate) fn is_bytecode(&self) -> bool {
        matches!(self.code, CompiledCode::Bytecode(_))
    }

    #[cfg(test)]
    pub(crate) fn source_text(&self) -> Option<&str> {
        match &self.code {
            CompiledCode::Source(code) => Some(code.as_ref()),
            CompiledCode::Bytecode(_) => None,
        }
    }
}

/// 一次改动之后，引擎那头要做的。
#[derive(Clone, Debug)]
pub enum Change {
    /// 起一个新引擎：外壳，加上此刻装着的全部 VM。
    Start {
        shell: CompiledCode,
        plugins: Vec<Compiled>,
    },
    /// 装上或者原地热重载一个 VM。
    Install(Compiled),
    /// 卸掉一个 VM。
    Remove(String),
}

const FNV1A_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

fn fnv1a_feed(hash: &mut u64, bytes: &[u8]) {
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    for &b in bytes {
        *hash ^= u64::from(b);
        *hash = hash.wrapping_mul(PRIME);
    }
    *hash ^= 0xff;
    *hash = hash.wrapping_mul(PRIME);
}

/// 插件目录在打包、指纹计算与发行拷贝时统一跳过的条目（避免源码之外的测试/产物/隐藏目录干扰）。
pub(crate) fn is_ignored_plugin_entry(name: &str, is_dir: bool) -> bool {
    name.starts_with('.')
        || name == "node_modules"
        || name == "tsconfig.json"
        || (is_dir && name == "tests")
        || (!is_dir
            && (name.contains(".test.")
                || name == "bundle.nshc"
                || name == "bundle.meta.json"
                || name == "builtins.nshc"
                || name == "builtins.meta.json"))
}

/// 共享 JS 模块插件声明的包描述。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModulePackage {
    pub id: String,
    pub specifier: String,
    pub root: PathBuf,
    pub entry: String,
    pub types: Option<String>,
    pub fingerprint: String,
    pub source: crate::manifest::PluginSource,
}

pub(crate) enum CompileMode<'a> {
    Plugin,
    Module { self_specifier: &'a str },
}

pub(crate) struct CompiledSource {
    pub code: String,
    pub used_modules: BTreeMap<String, String>,
    pub used_module_ids: Vec<String>,
}

pub(crate) struct BuiltinCompileInput<'a> {
    pub id: &'a str,
    pub root: &'a Path,
    pub entry_rel: &'a str,
}

pub(crate) struct CompiledBuiltinsSource {
    pub code: String,
    pub used_modules: BTreeMap<String, String>,
    pub per_plugin_module_ids: BTreeMap<String, Vec<String>>,
}

/// 校验共享模块声明的 import 标识符（如 `smelt/conversation`、`custom/my.conv`），禁止占用内置保留模块。
pub(crate) fn valid_module_specifier(spec: &str) -> bool {
    if spec == "nsh"
        || spec.starts_with("nsh/")
        || matches!(spec, "smelt/sdk" | "smelt/host" | "smelt/slots")
    {
        return false;
    }
    let mut count = 0usize;
    for seg in spec.split('/') {
        if !valid_id(seg) {
            return false;
        }
        count += 1;
    }
    count >= 2
}

/// 为共享模块插件内容计算确定性指纹（用于消费方 `.nshc` 失效检测与增量重编）。
pub(crate) fn module_fingerprint(
    root: &Path,
    specifier: &str,
    entry: &str,
) -> Result<String, String> {
    let files = read_dir(root).map_err(|error| format!("{}: {error}", root.display()))?;
    let mut hash = FNV1A_OFFSET;
    fnv1a_feed(&mut hash, specifier.as_bytes());
    fnv1a_feed(&mut hash, entry.as_bytes());
    for (rel, bytes) in files {
        fnv1a_feed(
            &mut hash,
            rel.to_string_lossy().replace('\\', "/").as_bytes(),
        );
        fnv1a_feed(&mut hash, &bytes);
    }
    Ok(format!("{hash:016x}"))
}

/// 对嵌入所有 Guest 插件的 SDK、配置与 QuickJS 字节码版本生成稳定指纹，
/// 避免升级 Smelt SDK 或 NSH 引擎版本后误读旧的 `.nshc` 预编译产物。
pub(crate) fn sdk_fingerprint() -> String {
    let mut hash = FNV1A_OFFSET;
    fnv1a_feed(&mut hash, nsh_js::BYTECODE_VERSION.as_bytes());
    fnv1a_feed(&mut hash, PLUGIN_NSH_CONFIG.as_bytes());
    fnv1a_feed(&mut hash, PROVIDER.as_bytes());
    fnv1a_feed(&mut hash, SDK.as_bytes());
    format!("{hash:016x}")
}

/// 装着的插件（编译与暂存层，运行在后台线程）。
pub struct Plugins {
    stage: PathBuf,
    ui_sets: Vec<SetDescription>,
    modules: Vec<ModulePackage>,
    shell: CompiledCode,
    installed: BTreeMap<String, Compiled>,
}

impl Plugins {
    /// `stage` 是只归它用的目录，打包时摆文件用；`shell` 是导出 `Shell` 的 TSX，插件摆在它画的
    /// `Slot` 里；`ui_sets` 和交给 `set_ui_sets` 的一致。
    pub fn new(stage: &Path, shell: &str, ui_sets: Vec<SetDescription>) -> Result<Self, String> {
        let io = |error: io::Error| format!("{}: {error}", stage.display());
        fs::create_dir_all(stage).map_err(io)?;
        let stage = stage.canonicalize().map_err(io)?;
        let dir = stage.join("shell");
        let files = [
            ("nsh.json", SHELL_NSH_CONFIG),
            ("App.tsx", APP),
            ("provider.tsx", PROVIDER),
            ("slots.tsx", SLOTS),
            ("sdk.tsx", SDK),
            ("Shell.tsx", shell),
        ];
        write_dir(
            &dir,
            files
                .into_iter()
                .map(|(path, text)| (Path::new(path), text.as_bytes())),
        )
        .map_err(io)?;
        let bundle = bundle(&dir.join("App.tsx"), &ui_sets, false)?;
        let served = native_specifiers(&ui_sets);
        validate_imports(SHELL_OWNER, &bundle.code, &served)?;
        Ok(Self {
            shell: CompiledCode::Source(bundle.code.into()),
            stage,
            ui_sets,
            modules: Vec::new(),
            installed: BTreeMap::new(),
        })
    }

    /// 直接使用构建期预编译的主壳 QuickJS 字节码构造；延迟建立 `stage`（全命中预编译插件时不落盘）。
    pub(crate) fn new_with_bytecode(
        stage: &Path,
        shell_bytecode: &'static [u8],
        ui_sets: Vec<SetDescription>,
    ) -> Result<Self, String> {
        Ok(Self {
            stage: stage.to_path_buf(),
            ui_sets,
            modules: Vec::new(),
            shell: CompiledCode::Bytecode(Arc::from(shell_bytecode)),
            installed: BTreeMap::new(),
        })
    }

    pub fn set_modules(&mut self, modules: Vec<ModulePackage>) {
        self.modules = modules;
    }

    pub(crate) fn stage(&self) -> &Path {
        &self.stage
    }

    pub(crate) fn ui_sets(&self) -> &[SetDescription] {
        &self.ui_sets
    }

    #[cfg(test)]
    pub(crate) fn shell_source(&self) -> Option<&str> {
        match &self.shell {
            CompiledCode::Source(src) => Some(src),
            CompiledCode::Bytecode(_) => None,
        }
    }

    /// 起引擎：外壳加上此刻装着的全部插件。
    pub fn start(&self) -> Change {
        Change::Start {
            shell: self.shell.clone(),
            plugins: self.installed.values().cloned().collect(),
        }
    }

    /// 装一个插件，或者它的文件变了。打不过包就什么都不变，它上一份照常跑。
    pub fn install(&mut self, id: &str, dir: &Path) -> Result<Change, String> {
        self.install_entry(id, dir, ENTRY)
    }

    /// 装一个指定入口文件（相对插件目录）的插件。
    pub fn install_entry(
        &mut self,
        id: &str,
        dir: &Path,
        entry_rel: &str,
    ) -> Result<Change, String> {
        let compiled = self.compile(id, dir, entry_rel)?;
        Ok(self.insert_compiled(compiled))
    }

    pub(crate) fn insert_compiled(&mut self, compiled: Compiled) -> Change {
        self.installed
            .insert(compiled.id().to_owned(), compiled.clone());
        Change::Install(compiled)
    }

    /// 装着的 VM id（共享内置 VM 记为 `BUILTINS_VM_ID`，独立插件记为插件 id）。
    pub fn installed(&self) -> impl Iterator<Item = &str> {
        self.installed.keys().map(String::as_str)
    }

    pub fn remove(&mut self, id: &str) -> Change {
        self.installed.remove(id);
        Change::Remove(id.to_owned())
    }

    /// 检查单插件是否声明/引用了 `specs` 中的任一共享模块。
    pub(crate) fn plugin_uses_any_module(
        root: &Path,
        modules: &[ModulePackage],
        specs: &BTreeSet<String>,
    ) -> bool {
        if specs.is_empty() {
            return false;
        }
        let Ok(files) = read_dir(root) else {
            return false;
        };
        let Ok(selected) = select_candidate_modules(&files, modules, None) else {
            return false;
        };
        selected
            .iter()
            .any(|item| specs.contains(&item.package.specifier))
    }

    /// 无共享可变状态地把单个插件（或共享模块插件）打包为 JS 文本，可由上层多线程并行调用。
    pub(crate) fn compile_source_to_js(
        stage: &Path,
        ui_sets: &[SetDescription],
        modules: &[ModulePackage],
        id: &str,
        source: &Path,
        entry_rel: &str,
        mode: CompileMode<'_>,
        minify: bool,
    ) -> Result<CompiledSource, String> {
        if !valid_id(id) {
            return Err(format!("插件 id 不能作目录名：{id}"));
        }
        let normalized_entry = normalize_rel_path(id, entry_rel)?;
        let files = read_dir(source).map_err(|error| format!("{}: {error}", source.display()))?;
        if !files.contains_key(&normalized_entry) {
            return Err(format!("{id} 没有 {entry_rel}"));
        }
        let import_rel = normalized_entry.to_string_lossy().replace('\\', "/");
        let raw_dir = stage.join("plugins").join(id);
        let entry = match mode {
            CompileMode::Plugin => format!(
                "import {{ provideViews }} from \"./provider.tsx\";\n\
                 import * as views from \"./plugin/{import_rel}\";\n\
                 provideViews({id:?}, views);\n"
            ),
            CompileMode::Module { .. } => {
                format!(
                    "import * as _module_exports from \"./plugin/{import_rel}\";\n\
                     (globalThis as Record<string, unknown>).__nsh_module_exports = _module_exports;\n"
                )
            }
        };

        let self_specifier = match mode {
            CompileMode::Plugin => None,
            CompileMode::Module { self_specifier } => Some(self_specifier),
        };
        let selected = select_candidate_modules(&files, modules, self_specifier)?;
        let nsh_config_bytes = build_plugin_nsh_config(&selected, true, id)?;

        let mut staged: Vec<(PathBuf, &[u8])> = files
            .iter()
            .map(|(path, bytes)| (Path::new("plugin").join(path), bytes.as_slice()))
            .chain([
                (PathBuf::from("nsh.json"), nsh_config_bytes.as_slice()),
                (PathBuf::from("provider.tsx"), PROVIDER.as_bytes()),
                (PathBuf::from("sdk.tsx"), SDK.as_bytes()),
                (PathBuf::from("entry.ts"), entry.as_bytes()),
            ])
            .collect();
        for item in &selected {
            let prefix = Path::new("modules").join(&item.package.id);
            staged.extend(
                item.files
                    .iter()
                    .map(|(path, bytes)| (prefix.join(path), bytes.as_slice())),
            );
        }

        let (dir, bundle) = stage_and_bundle(&raw_dir, id, &staged, ui_sets, minify)?;
        let hit_ids = collect_hit_modules(&dir, &bundle.watch_files);
        let mut used_modules = BTreeMap::new();
        let mut used_module_ids = Vec::new();
        for item in &selected {
            if hit_ids.contains(&item.package.id) {
                used_modules.insert(
                    item.package.specifier.clone(),
                    item.package.fingerprint.clone(),
                );
                used_module_ids.push(item.package.id.clone());
            }
        }
        used_module_ids.sort();
        used_module_ids.dedup();

        Ok(CompiledSource {
            code: bundle.code,
            used_modules,
            used_module_ids,
        })
    }

    /// 把一组内置脚本插件打进同一份共享 ES 模块包（共用 `sdk.tsx` / `provider.tsx` 与共享模块），
    /// 并为每个子插件经就近 `node_modules/smelt/sdk.ts` 注入按 `id` 隔离的作用域 SDK。
    pub(crate) fn compile_builtins_to_js(
        stage: &Path,
        ui_sets: &[SetDescription],
        modules: &[ModulePackage],
        entries: &[BuiltinCompileInput<'_>],
        minify: bool,
    ) -> Result<CompiledBuiltinsSource, String> {
        if entries.is_empty() {
            return Err("compile_builtins_to_js：内置插件列表不能为空".to_owned());
        }

        struct PreparedEntry<'a> {
            id: &'a str,
            import_rel: String,
            files: BTreeMap<PathBuf, Vec<u8>>,
            shim: String,
            candidate_mod_ids: Vec<String>,
        }

        let mut prepared = Vec::with_capacity(entries.len());
        let mut all_selected: BTreeMap<String, SelectedModule<'_>> = BTreeMap::new();
        let mut seen_ids = BTreeSet::new();

        for item in entries {
            if !valid_id(item.id) {
                return Err(format!("插件 id 不能作目录名：{}", item.id));
            }
            if !seen_ids.insert(item.id) {
                return Err(format!("builtins 合包存在重复插件 id：{}", item.id));
            }
            let normalized_entry = normalize_rel_path(item.id, item.entry_rel)?;
            let files =
                read_dir(item.root).map_err(|error| format!("{}: {error}", item.root.display()))?;
            if !files.contains_key(&normalized_entry) {
                return Err(format!("{} 没有 {}", item.id, item.entry_rel));
            }
            let import_rel = normalized_entry.to_string_lossy().replace('\\', "/");
            let selected = select_candidate_modules(&files, modules, None)?;
            let mut candidate_mod_ids = Vec::with_capacity(selected.len());
            for m in selected {
                candidate_mod_ids.push(m.package.id.clone());
                all_selected.entry(m.package.id.clone()).or_insert(m);
            }
            prepared.push(PreparedEntry {
                id: item.id,
                import_rel,
                files,
                shim: scoped_sdk_shim(item.id),
                candidate_mod_ids,
            });
        }

        let selected_list: Vec<SelectedModule<'_>> = all_selected.into_values().collect();
        let nsh_config_bytes = build_plugin_nsh_config(&selected_list, false, BUILTINS_VM_ID)?;

        let mut entry_src = String::from("import { provideMultiViews } from \"./provider.tsx\";\n");
        for (ix, item) in prepared.iter().enumerate() {
            let id = item.id;
            let rel = &item.import_rel;
            entry_src.push_str(&format!(
                "import * as p_{ix} from \"./plugins/{id}/plugin/{rel}\";\n"
            ));
        }
        entry_src.push_str("provideMultiViews({\n");
        for (ix, item) in prepared.iter().enumerate() {
            entry_src.push_str(&format!("  {:?}: p_{ix},\n", item.id));
        }
        entry_src.push_str("});\n");

        let shared_sdk_shim = b"export * from \"../../sdk.tsx\";\n";
        let mut staged: Vec<(PathBuf, &[u8])> = vec![
            (PathBuf::from("nsh.json"), nsh_config_bytes.as_slice()),
            (PathBuf::from("provider.tsx"), PROVIDER.as_bytes()),
            (PathBuf::from("sdk.tsx"), SDK.as_bytes()),
            (
                PathBuf::from("node_modules/smelt/sdk.ts"),
                shared_sdk_shim.as_slice(),
            ),
            (PathBuf::from("entry.ts"), entry_src.as_bytes()),
        ];
        for item in &selected_list {
            let prefix = Path::new("modules").join(&item.package.id);
            staged.extend(
                item.files
                    .iter()
                    .map(|(path, bytes)| (prefix.join(path), bytes.as_slice())),
            );
        }
        for item in &prepared {
            let base = Path::new("plugins").join(item.id);
            let plugin_prefix = base.join("plugin");
            staged.extend(
                item.files
                    .iter()
                    .map(|(path, bytes)| (plugin_prefix.join(path), bytes.as_slice())),
            );
            staged.push((base.join("node_modules/smelt/sdk.ts"), item.shim.as_bytes()));
        }

        let raw_dir = stage.join("builtins");
        let (dir, bundle) = stage_and_bundle(&raw_dir, BUILTINS_VM_ID, &staged, ui_sets, minify)?;
        let hit_ids = collect_hit_modules(&dir, &bundle.watch_files);

        let mut used_modules = BTreeMap::new();
        for item in &selected_list {
            if hit_ids.contains(&item.package.id) {
                used_modules.insert(
                    item.package.specifier.clone(),
                    item.package.fingerprint.clone(),
                );
            }
        }
        let mut per_plugin_module_ids = BTreeMap::new();
        for item in &prepared {
            let mut ids: Vec<String> = item
                .candidate_mod_ids
                .iter()
                .filter(|id| hit_ids.contains(*id))
                .cloned()
                .collect();
            ids.sort();
            ids.dedup();
            per_plugin_module_ids.insert(item.id.to_owned(), ids);
        }

        Ok(CompiledBuiltinsSource {
            code: bundle.code,
            used_modules,
            per_plugin_module_ids,
        })
    }

    /// 拷一份到自己的目录里再打包：插件目录里不落生成的文件，import 也出不了这个目录。
    fn compile(&self, id: &str, source: &Path, entry_rel: &str) -> Result<Compiled, String> {
        let compiled = Self::compile_source_to_js(
            &self.stage,
            &self.ui_sets,
            &self.modules,
            id,
            source,
            entry_rel,
            CompileMode::Plugin,
            false,
        )?;
        Ok(Compiled::from_source(id.to_owned(), compiled.code))
    }
}

fn build_plugin_nsh_config(
    selected: &[SelectedModule<'_>],
    include_sdk_alias: bool,
    owner: &str,
) -> Result<Vec<u8>, String> {
    if selected.is_empty() && include_sdk_alias {
        return Ok(PLUGIN_NSH_CONFIG.as_bytes().to_vec());
    }
    let mut config: Value = serde_json::from_str(PLUGIN_NSH_CONFIG)
        .map_err(|error| format!("解析内置 plugin.nsh.json 失败：{error}"))?;
    let alias = config
        .get_mut("resolve")
        .and_then(|v| v.get_mut("alias"))
        .and_then(Value::as_object_mut)
        .ok_or_else(|| "内置 plugin.nsh.json 缺少 resolve.alias".to_owned())?;
    if !include_sdk_alias {
        alias.remove("smelt/sdk");
    }
    for item in selected {
        alias.insert(
            item.package.specifier.clone(),
            Value::String(format!("./modules/{}/{}", item.package.id, item.entry_rel)),
        );
    }
    serde_json::to_vec(&config).map_err(|error| format!("序列化 {owner} nsh.json 失败：{error}"))
}

fn stage_and_bundle(
    raw_dir: &Path,
    owner: &str,
    staged: &[(PathBuf, &[u8])],
    ui_sets: &[SetDescription],
    minify: bool,
) -> Result<(PathBuf, nsh_bundler::Bundle), String> {
    write_dir(
        raw_dir,
        staged.iter().map(|(path, bytes)| (path.as_path(), *bytes)),
    )
    .map_err(|error| format!("{}: {error}", raw_dir.display()))?;
    // macOS `/var` -> `/private/var` 符号链接解析，确保 `inside` 前缀对比一致。
    let dir = raw_dir
        .canonicalize()
        .map_err(|error| format!("{}: {error}", raw_dir.display()))?;
    let bundle = bundle(&dir.join("entry.ts"), ui_sets, minify)?;
    if let Some(outside) = bundle.watch_files.iter().find(|file| !inside(file, &dir)) {
        return Err(format!(
            "{owner} 引了自己目录以外的文件：{}",
            outside.display()
        ));
    }
    if let Some(asset) = bundle.assets.first() {
        return Err(format!(
            "{owner} 引了资源文件，还不支持：{}",
            asset.display()
        ));
    }
    let served = native_specifiers(ui_sets);
    validate_imports(owner, &bundle.code, &served)?;
    Ok((dir, bundle))
}

fn collect_hit_modules(dir: &Path, watch_files: &[PathBuf]) -> BTreeSet<String> {
    let modules_dir = dir.join("modules");
    let mut hit_ids = BTreeSet::new();
    for watched in watch_files {
        if let Ok(rel) = watched.strip_prefix(&modules_dir)
            && let Some(Component::Normal(first)) = rel.components().next()
            && let Some(mod_id) = first.to_str()
        {
            hit_ids.insert(mod_id.to_owned());
        }
    }
    hit_ids
}

fn scoped_sdk_shim(id: &str) -> String {
    format!(
        "export * from \"../../../../sdk.tsx\";\n\
         import {{ createScopedSdk }} from \"../../../../provider.tsx\";\n\
         const _s = createScopedSdk({id:?});\n\
         export const pluginId = _s.pluginId;\n\
         export const dataDir = _s.dataDir;\n\
         export const bootTask = _s.bootTask;\n\
         export const t = _s.t;\n\
         export const text = _s.text;\n\
         export const texts = _s.texts;\n\
         export const onEvent = _s.onEvent;\n\
         export const onCommand = _s.onCommand;\n\
         export const store = _s.store;\n\
         export const registerViews = _s.registerViews;\n\
         export const registerCommands = _s.registerCommands;\n\
         export const registerTopics = _s.registerTopics;\n\
         export const registerLayouts = _s.registerLayouts;\n\
         export const registerPages = _s.registerPages;\n\
         export const registerContextMenuItems = _s.registerContextMenuItems;\n\
         export const registerMenuItems = _s.registerMenuItems;\n\
         export const registerFileOpener = _s.registerFileOpener;\n\
         export const setStatusMenu = _s.setStatusMenu;\n\
         export const emitEvent = _s.emitEvent;\n"
    )
}

struct SelectedModule<'a> {
    package: &'a ModulePackage,
    files: BTreeMap<PathBuf, Vec<u8>>,
    entry_rel: String,
}

fn normalize_rel_path(owner: &str, entry_rel: &str) -> Result<PathBuf, String> {
    let rel = Path::new(entry_rel);
    if rel.is_absolute()
        || rel.components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(format!("{owner} 的入口路径不合法：{entry_rel}"));
    }
    Ok(rel
        .components()
        .filter(|part| !matches!(part, Component::CurDir))
        .collect())
}

fn is_source_file(path: &Path) -> bool {
    const EXTS: &[&str] = &["ts", "tsx", "js", "jsx", "mjs", "cjs"];
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| EXTS.iter().any(|allowed| ext.eq_ignore_ascii_case(allowed)))
}

fn contains_specifier(files: &BTreeMap<PathBuf, Vec<u8>>, specifier: &str) -> bool {
    let needle = specifier.as_bytes();
    !needle.is_empty()
        && files
            .iter()
            .filter(|(path, _)| is_source_file(path))
            .any(|(_, bytes)| bytes.windows(needle.len()).any(|w| w == needle))
}

fn select_candidate_modules<'a>(
    root_files: &BTreeMap<PathBuf, Vec<u8>>,
    modules: &'a [ModulePackage],
    self_specifier: Option<&str>,
) -> Result<Vec<SelectedModule<'a>>, String> {
    if modules.is_empty() {
        return Ok(Vec::new());
    }
    let mut selected_indices = BTreeSet::new();
    let mut queue = Vec::new();
    for (ix, m) in modules.iter().enumerate() {
        if Some(m.specifier.as_str()) != self_specifier
            && contains_specifier(root_files, &m.specifier)
        {
            selected_indices.insert(ix);
            queue.push(ix);
        }
    }
    let mut selected = Vec::with_capacity(queue.len());
    let mut head = 0usize;
    while let Some(&ix) = queue.get(head) {
        head += 1;
        let pkg = &modules[ix];
        let m_files =
            read_dir(&pkg.root).map_err(|error| format!("{}: {error}", pkg.root.display()))?;
        let m_entry = normalize_rel_path(&pkg.id, &pkg.entry)?;
        if !m_files.contains_key(&m_entry) {
            return Err(format!("模块插件 {} 缺少入口 {}", pkg.id, pkg.entry));
        }
        let entry_rel = m_entry.to_string_lossy().replace('\\', "/");
        for (other_ix, other) in modules.iter().enumerate() {
            if !selected_indices.contains(&other_ix)
                && Some(other.specifier.as_str()) != self_specifier
                && contains_specifier(&m_files, &other.specifier)
            {
                selected_indices.insert(other_ix);
                queue.push(other_ix);
            }
        }
        selected.push(SelectedModule {
            package: pkg,
            files: m_files,
            entry_rel,
        });
    }
    Ok(selected)
}

/// 引擎内插件运行时与跨 VM 插槽路由器。
///
/// 每个插件独享一条 [`HostChannel`]，回调拿到的 `plugin_id` 由 Rust 闭包绑定，插件无法伪造；
/// 跨 VM 插槽的挂载、参数更新、卸载与热重载重放全部在引擎线程内直转，不回绕 GPUI 主线程。
#[derive(Clone, Default)]
pub struct PluginHost(Arc<Mutex<HostInner>>);

type CallHandler = Arc<dyn Fn(&str, HostCall) + Send + Sync>;

/// 插件 VM 生命周期事件（在引擎线程触发，经 channel 转主线程处理）。
#[derive(Clone, Debug)]
pub enum PluginLifecycle {
    Ready { plugin: String, views: Vec<String> },
    Fatal { plugin: String, message: String },
}

type LifecycleObserver = Arc<dyn Fn(PluginLifecycle) + Send + Sync>;

struct PluginEntry {
    generation: u64,
    channel: HostChannel,
    members: Vec<String>,
    ready_members: BTreeSet<String>,
}

#[derive(Clone)]
struct SlotInstance {
    target: String,
    plugin: String,
    view: String,
    args: Value,
}

#[derive(Default)]
struct HostInner {
    shell_channel: Option<HostChannel>,
    next_generation: u64,
    plugins: BTreeMap<String, PluginEntry>,
    vm_by_plugin: BTreeMap<String, String>,
    instances: BTreeMap<String, SlotInstance>,
    failed: BTreeSet<String>,
    handlers: BTreeMap<String, CallHandler>,
    lifecycle: Option<LifecycleObserver>,
    plugin_limits: Option<Limits>,
    /// 默认关闭按墙钟计时的 1s 滚动 CPU 窗（`None`）：macOS 休眠唤醒、电池降频或线程抢占会使单次
    /// 轻量回调的墙钟耗时越过 250ms 而误杀健康插件（如 `smelt.layouts`）。死循环与内存耗尽仍由
    /// `Limits::default()` 的 `call_budget` / `load_budget` / `memory_bytes` / `max_nodes` 熔断保护。
    cpu_window_budget: Option<Duration>,
}

impl PluginHost {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, HostInner> {
        self.0.lock().expect("plugin host lock poisoned")
    }

    /// 覆写 Guest 插件 VM 的资源上限（默认沿用 `PluginConfig::new` 的加固默认值）。
    pub fn set_plugin_limits(&self, limits: Limits) {
        self.lock().plugin_limits = Some(limits);
    }

    /// 覆写 Guest 插件 1 秒滚动 CPU 配额（`None` 关闭滚动窗）。
    pub fn set_cpu_window_budget(&self, budget: Option<Duration>) {
        self.lock().cpu_window_budget = budget;
    }

    /// 登记一条给所有插件调的宿主函数；`handler` 在引擎线程上跑，第一个参数是不可伪造的插件 id。
    pub fn handle(
        &self,
        name: impl Into<String>,
        handler: impl Fn(&str, HostCall) + Send + Sync + 'static,
    ) {
        self.lock().handlers.insert(name.into(), Arc::new(handler));
    }

    /// 把一条宿主事件广播给此刻存活的全部插件 VM。
    pub fn broadcast(&self, event: &str, data: Value) {
        let channels: Vec<HostChannel> = self
            .lock()
            .plugins
            .values()
            .map(|entry| entry.channel.clone())
            .collect();
        for channel in channels {
            if let Err(error) = channel.send(event, data.clone()) {
                eprintln!("[nsh-plugins] 广播 {event} 失败：{error}");
            }
        }
    }

    /// 把一条宿主事件广播给外壳和此刻存活的全部插件 VM。
    pub fn broadcast_all(&self, event: &str, data: Value) {
        let channels: Vec<HostChannel> = {
            let inner = self.lock();
            inner
                .shell_channel
                .iter()
                .cloned()
                .chain(inner.plugins.values().map(|entry| entry.channel.clone()))
                .collect()
        };
        for channel in channels {
            if let Err(error) = channel.send(event, data.clone()) {
                eprintln!("[nsh-plugins] 广播 {event} 失败：{error}");
            }
        }
    }

    /// 登记插件 VM 生命周期观察者（就绪时拿到导出的视图列表，致命故障时清理宿主登记）。
    pub fn set_lifecycle_observer(
        &self,
        observer: impl Fn(PluginLifecycle) + Send + Sync + 'static,
    ) {
        self.lock().lifecycle = Some(Arc::new(observer));
    }

    /// 把一条事件只推给外壳 VM。
    pub fn send_to_shell(&self, event: &str, data: Value) {
        let channel = self.lock().shell_channel.clone();
        if let Some(channel) = channel
            && let Err(error) = channel.send(event, data)
        {
            eprintln!("[nsh-plugins] 推送 {event} 给外壳失败：{error}");
        }
    }

    /// 把一条宿主事件只推给指定的插件（或共享 VM `target`）。
    pub fn send_to(&self, target: &str, event: &str, data: Value) {
        let channel = {
            let inner = self.lock();
            inner
                .plugins
                .get(target)
                .or_else(|| {
                    inner
                        .vm_by_plugin
                        .get(target)
                        .and_then(|vm| inner.plugins.get(vm))
                })
                .map(|entry| entry.channel.clone())
        };
        if let Some(channel) = channel
            && let Err(error) = channel.send(event, data)
        {
            eprintln!("[nsh-plugins] 推送 {event} 给 {target} 失败：{error}");
        }
    }

    pub(crate) fn vm_of(&self, plugin: &str) -> String {
        self.lock()
            .vm_by_plugin
            .get(plugin)
            .cloned()
            .unwrap_or_else(|| plugin.to_owned())
    }

    pub(crate) fn members_of(&self, vm_id: &str) -> Vec<String> {
        self.lock()
            .plugins
            .get(vm_id)
            .map(|entry| entry.members.clone())
            .unwrap_or_default()
    }

    pub fn vm_ids(&self) -> Vec<String> {
        self.lock().plugins.keys().cloned().collect()
    }

    /// 把一次变更交给引擎：外壳和插件各自运行在 `harden: true` 的加固 QuickJS VM 中。
    pub fn apply(&self, view: &AppView, change: Change) {
        match change {
            Change::Start { shell, plugins } => {
                let shell_channel = view.host();
                let (generation, handlers) = {
                    let mut inner = self.lock();
                    inner.next_generation += 1;
                    inner.shell_channel = Some(shell_channel.clone());
                    inner.plugins.clear();
                    inner.vm_by_plugin.clear();
                    inner.instances.clear();
                    inner.failed.clear();
                    let handlers: Vec<(String, CallHandler)> = inner
                        .handlers
                        .iter()
                        .map(|(name, f)| (name.clone(), f.clone()))
                        .collect();
                    (inner.next_generation, handlers)
                };
                let mut params = serde_json::Map::new();
                params.insert(
                    "__smelt_owner".into(),
                    Value::String(format!("shell:{generation}")),
                );
                shell_channel.set_params(params);
                self.attach_slot_ops(&shell_channel);

                let mut declared_host = vec![SLOT_OPS.to_owned()];
                for (name, handler) in handlers {
                    declared_host.push(name.clone());
                    shell_channel.handle(name, move |call| handler(SHELL_OWNER, call));
                }

                let mut policy = Policy::default();
                let mut declared = Capabilities::all();
                declared.host = declared_host;
                policy.declared = Some(declared);
                view.load(App::Js(make_bundle(&shell, policy)));
                for plugin in plugins {
                    self.load_one(view, plugin);
                }
            }
            Change::Install(plugin) => self.load_one(view, plugin),
            Change::Remove(id) => {
                let (shell_channel, cleared_failed) = {
                    let mut inner = self.lock();
                    let removed_members = inner
                        .plugins
                        .remove(&id)
                        .map(|entry| entry.members)
                        .unwrap_or_else(|| vec![id.clone()]);
                    let mut cleared = Vec::new();
                    for member in removed_members {
                        if inner.vm_by_plugin.get(&member).is_some_and(|v| v == &id) {
                            inner.vm_by_plugin.remove(&member);
                        }
                        if inner.failed.remove(&member) {
                            cleared.push(member);
                        }
                    }
                    (inner.shell_channel.clone(), cleared)
                };
                if let Some(shell_channel) = shell_channel {
                    for member in cleared_failed {
                        send_failed_status(&shell_channel, &member, false);
                    }
                }
                view.unload_plugin(id);
            }
        }
    }

    /// 处理引擎报出的 [`AppError`]：若是某个插件（或共享 VM）的致命错误（顶层抛错、OOM、死循环熔断等），
    /// 仅收尾该 VM 名下成员的路由表并标出故障状态，绝不重启引擎或影响其它 VM。
    pub fn on_app_error(&self, error: &AppError) {
        let Some(vm_id) = error.plugin.as_deref() else {
            return;
        };
        if !error.is_fatal() {
            return;
        }
        let (members, shell_channel, observer) = {
            let mut inner = self.lock();
            // 若新版本已经加载完成并报就绪，则忽略上一代延迟到达的 Load 错误。
            if error.kind == AppErrorKind::Load
                && inner
                    .plugins
                    .get(vm_id)
                    .is_some_and(|entry| !entry.ready_members.is_empty())
            {
                return;
            }
            let members = inner
                .plugins
                .remove(vm_id)
                .map(|entry| entry.members)
                .unwrap_or_else(|| vec![vm_id.to_owned()]);
            for member in &members {
                if inner.vm_by_plugin.get(member).is_some_and(|v| v == vm_id) {
                    inner.vm_by_plugin.remove(member);
                }
                inner.failed.insert(member.clone());
            }
            (
                members,
                inner.shell_channel.clone(),
                inner.lifecycle.clone(),
            )
        };
        let message = error.to_string();
        for member in members {
            if let Some(shell_channel) = shell_channel.as_ref() {
                send_failed_status(shell_channel, &member, true);
            }
            if let Some(observer) = observer.as_ref() {
                observer(PluginLifecycle::Fatal {
                    plugin: member,
                    message: message.clone(),
                });
            }
        }
    }

    fn load_one(&self, view: &AppView, plugin: Compiled) {
        let vm_id = plugin.id;
        let members = plugin.members;
        let is_builtins = vm_id == BUILTINS_VM_ID;
        let channel = HostChannel::new();
        let (generation, handlers, limits, cpu_window, clear_failed_shell, cleared_failed) = {
            let mut inner = self.lock();
            inner.next_generation += 1;
            let generation = inner.next_generation;
            if let Some(prev) = inner.plugins.remove(&vm_id) {
                for old_member in prev.members {
                    if inner
                        .vm_by_plugin
                        .get(&old_member)
                        .is_some_and(|v| v == &vm_id)
                    {
                        inner.vm_by_plugin.remove(&old_member);
                    }
                }
            }
            let mut cleared_failed = Vec::new();
            for member in &members {
                if inner.failed.remove(member) {
                    cleared_failed.push(member.clone());
                }
                inner.vm_by_plugin.insert(member.clone(), vm_id.clone());
            }
            inner.plugins.insert(
                vm_id.clone(),
                PluginEntry {
                    generation,
                    channel: channel.clone(),
                    members: members.clone(),
                    ready_members: BTreeSet::new(),
                },
            );
            let handlers: Vec<(String, CallHandler)> = inner
                .handlers
                .iter()
                .map(|(name, f)| (name.clone(), f.clone()))
                .collect();
            let clear_failed_shell = (!cleared_failed.is_empty())
                .then(|| inner.shell_channel.clone())
                .flatten();
            (
                generation,
                handlers,
                inner.plugin_limits,
                inner.cpu_window_budget,
                clear_failed_shell,
                cleared_failed,
            )
        };

        if let Some(shell_channel) = clear_failed_shell {
            for member in cleared_failed {
                send_failed_status(&shell_channel, &member, false);
            }
        }

        let mut params = serde_json::Map::new();
        if is_builtins {
            let mut dirs = serde_json::Map::new();
            for member in &members {
                if let Ok(data_dir) = smelt_plugin_api::plugin_data_dir(member) {
                    dirs.insert(
                        member.clone(),
                        Value::String(data_dir.to_string_lossy().into_owned()),
                    );
                }
            }
            params.insert(
                "__smelt_builtin_members".into(),
                Value::Array(members.iter().cloned().map(Value::String).collect()),
            );
            params.insert("__smelt_data_dirs".into(), Value::Object(dirs));
        } else {
            let logical_id = members.first().map(String::as_str).unwrap_or(&vm_id);
            params.insert(
                "__smelt_plugin_id".into(),
                Value::String(logical_id.to_owned()),
            );
            if let Ok(data_dir) = smelt_plugin_api::plugin_data_dir(logical_id) {
                params.insert(
                    "__smelt_data_dir".into(),
                    Value::String(data_dir.to_string_lossy().into_owned()),
                );
            }
        }
        channel.set_params(params);

        let allowed_members = Arc::new(members.into_iter().collect::<BTreeSet<String>>());

        // 插件初始化完 provideViews / provideMultiViews 后上报就绪：宿主立即按外壳 instances 表原位重放。
        {
            let host = self.clone();
            let vm = vm_id.clone();
            let ready_channel = channel.clone();
            let allowed = allowed_members.clone();
            channel.handle(PLUGIN_READY, move |call| {
                let views: Vec<String> = call
                    .data()
                    .get("views")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                let member_id = if is_builtins {
                    let Some(p) = call
                        .data()
                        .get("plugin")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                    else {
                        return call.reject("smelt.plugin.ready：共享内置 VM 缺少 plugin 字段");
                    };
                    if !allowed.contains(&p) {
                        return call.reject(format!("smelt.plugin.ready：非法成员 {p}"));
                    }
                    p
                } else {
                    vm.clone()
                };
                let (replay, observer) = {
                    let mut inner = host.lock();
                    let Some(entry) = inner.plugins.get_mut(&vm) else {
                        return;
                    };
                    if entry.generation != generation {
                        return;
                    }
                    entry.ready_members.insert(member_id.clone());
                    let replay: Vec<Value> = inner
                        .instances
                        .iter()
                        .filter(|(_, inst)| inst.plugin == member_id)
                        .map(|(instance, inst)| {
                            json!({
                                "op": "mount",
                                "plugin": inst.plugin,
                                "instance": instance,
                                "target": inst.target,
                                "view": inst.view,
                                "args": inst.args,
                            })
                        })
                        .collect();
                    (replay, inner.lifecycle.clone())
                };
                if !replay.is_empty()
                    && let Err(error) = ready_channel.send(SLOT_SYNC, Value::Array(replay))
                {
                    eprintln!("[nsh-plugins] 重放 {member_id} 插槽失败：{error}");
                }
                if let Some(observer) = observer {
                    observer(PluginLifecycle::Ready {
                        plugin: member_id,
                        views,
                    });
                }
                call.resolve(Value::Null);
            });
        }

        // 绑定所有宿主函数：独立 VM 的 plugin_id 由 Rust 闭包固定、绝不可伪造；
        // 共享内置 VM 按请求体 `__plugin` 且必须命中 `allowed_members` 白名单分发。
        // Guest 插件不具备 SLOT_OPS 权限：只有宿主外壳能挂载插件视图。
        let mut declared_host = vec![PLUGIN_READY.to_owned()];
        for (name, handler) in handlers {
            declared_host.push(name.clone());
            if is_builtins {
                let allowed = allowed_members.clone();
                let vm = vm_id.clone();
                channel.handle(name, move |call| {
                    let caller = call
                        .data()
                        .get("__plugin")
                        .and_then(Value::as_str)
                        .filter(|id| allowed.contains(*id))
                        .map(str::to_owned)
                        .unwrap_or_else(|| vm.clone());
                    handler(&caller, call);
                });
            } else {
                let plugin_id = vm_id.clone();
                channel.handle(name, move |call| handler(&plugin_id, call));
            }
        }

        let mut policy = Policy::default();
        let mut declared = Capabilities::all();
        declared.host = declared_host;
        policy.declared = Some(declared);
        let bundle = make_bundle(&plugin.code, policy);
        let mut config = PluginConfig::new(vm_id, bundle)
            .with_host(channel)
            .with_cpu_window_budget(cpu_window);
        let effective_limits = limits.or_else(|| {
            is_builtins.then_some(Limits {
                memory_bytes: 256 << 20,
                load_budget: Duration::from_millis(2_000),
                call_budget: Duration::from_millis(250),
                max_nodes: 80_000,
                ..Limits::default()
            })
        });
        if let Some(limits) = effective_limits {
            config = config.with_limits(limits);
        }
        view.load_plugin(config);
    }

    fn attach_slot_ops(&self, channel: &HostChannel) {
        let host = self.clone();
        channel.handle(SLOT_OPS, move |mut call| {
            let Value::Array(ops) = call.take_data() else {
                return;
            };
            let outgoing = host.lock().apply_slot_ops(ops);
            flush_sync_batches(outgoing);
        });
    }
}

fn make_bundle(code: &CompiledCode, policy: Policy) -> Bundle {
    let bundle = match code {
        CompiledCode::Source(src) => Bundle::source(src.as_ref().to_owned()),
        CompiledCode::Bytecode(bytes) => {
            // SAFETY: 字节码仅来自构建期 `include_bytes!` 主壳产物，或发版前 `stage_builtin_plugins`
            // 产出的同版本、`sdk_fingerprint` 匹配的只读自带插件 `bundle.nshc`。
            unsafe { Bundle::bytecode(bytes.as_ref().to_vec()) }
        }
    };
    bundle.with_policy(policy)
}

impl HostInner {
    fn apply_slot_ops(&mut self, ops: Vec<Value>) -> Vec<(HostChannel, Vec<Value>)> {
        let mut by_target: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for mut item in ops {
            let Some(op) = item.get("op").and_then(Value::as_str).map(str::to_owned) else {
                continue;
            };
            let Some(instance) = item
                .get("instance")
                .and_then(Value::as_str)
                .map(str::to_owned)
            else {
                continue;
            };
            match op.as_str() {
                "mount" => {
                    let Some(target) = item
                        .get("target")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                    else {
                        continue;
                    };
                    let Some(plugin) = item
                        .get("plugin")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                    else {
                        continue;
                    };
                    let view = item
                        .get("view")
                        .and_then(Value::as_str)
                        .unwrap_or("default")
                        .to_owned();
                    let args = item.get_mut("args").map(Value::take).unwrap_or(Value::Null);
                    if let Some(prev) = self.instances.insert(
                        instance.clone(),
                        SlotInstance {
                            target: target.clone(),
                            plugin: plugin.clone(),
                            view: view.clone(),
                            args: args.clone(),
                        },
                    ) && prev.plugin != plugin
                    {
                        by_target
                            .entry(prev.plugin)
                            .or_default()
                            .push(json!({ "op": "unmount", "instance": instance }));
                    }
                    by_target.entry(plugin.clone()).or_default().push(json!({
                        "op": "mount",
                        "plugin": plugin,
                        "instance": instance,
                        "target": target,
                        "view": view,
                        "args": args,
                    }));
                }
                "update" => {
                    let args = item.get_mut("args").map(Value::take).unwrap_or(Value::Null);
                    let Some(inst) = self.instances.get_mut(&instance) else {
                        continue;
                    };
                    inst.args = args.clone();
                    by_target
                        .entry(inst.plugin.clone())
                        .or_default()
                        .push(json!({
                            "op": "update",
                            "instance": instance,
                            "args": args,
                        }));
                }
                "unmount" => {
                    if let Some(inst) = self.instances.remove(&instance) {
                        by_target
                            .entry(inst.plugin)
                            .or_default()
                            .push(json!({ "op": "unmount", "instance": instance }));
                    }
                }
                _ => {}
            }
        }
        self.resolve_ready_batches(by_target)
    }

    fn resolve_ready_batches(
        &self,
        by_target: BTreeMap<String, Vec<Value>>,
    ) -> Vec<(HostChannel, Vec<Value>)> {
        let mut by_vm: BTreeMap<String, (HostChannel, Vec<Value>)> = BTreeMap::new();
        for (target_plugin, mut batch) in by_target {
            if batch.is_empty() {
                continue;
            }
            if let Some(vm_id) = self.vm_by_plugin.get(&target_plugin)
                && let Some(entry) = self.plugins.get(vm_id)
                && entry.ready_members.contains(&target_plugin)
            {
                by_vm
                    .entry(vm_id.clone())
                    .or_insert_with(|| (entry.channel.clone(), Vec::new()))
                    .1
                    .append(&mut batch);
            }
        }
        by_vm.into_values().collect()
    }
}

fn flush_sync_batches(batches: Vec<(HostChannel, Vec<Value>)>) {
    for (channel, batch) in batches {
        if let Err(error) = channel.send(SLOT_SYNC, Value::Array(batch)) {
            eprintln!("[nsh-plugins] 下发插槽同步失败：{error}");
        }
    }
}

fn send_failed_status(channel: &HostChannel, plugin: &str, failed: bool) {
    let _ = channel.send(SLOT_FAILED, json!({ "plugin": plugin, "failed": failed }));
}

fn bundle(
    entry: &Path,
    ui_sets: &[SetDescription],
    minify: bool,
) -> Result<nsh_bundler::Bundle, String> {
    nsh_bundler::bundle(&BundleOptions {
        entry: entry.to_path_buf(),
        minify,
        development: !minify,
        ui_sets: ui_sets.to_vec(),
        ..Default::default()
    })
    .map_err(|error| error.plain)
}

/// 插件能 import 的宿主模块。
fn native_specifiers(ui_sets: &[SetDescription]) -> Vec<String> {
    nsh_core::js::NATIVE_MODULES
        .iter()
        .map(|name| (*name).to_owned())
        .chain(ui_sets.iter().map(|set| set.specifier.clone()))
        .collect()
}

/// QuickJS + NSH 沙箱提供的合法全局标识符；打包产物里除此以外的未解析自由标识符多半是漏 import 或拼写错误。
const ALLOWED_GLOBALS: &[&str] = &[
    // ECMAScript 标准全局
    "AggregateError",
    "Array",
    "ArrayBuffer",
    "Atomics",
    "BigInt",
    "BigInt64Array",
    "BigUint64Array",
    "Boolean",
    "DataView",
    "Date",
    "Error",
    "EvalError",
    "FinalizationRegistry",
    "Float16Array",
    "Float32Array",
    "Float64Array",
    "Function",
    "Infinity",
    "Int8Array",
    "Int16Array",
    "Int32Array",
    "Intl",
    "JSON",
    "Map",
    "Math",
    "NaN",
    "Number",
    "Object",
    "Promise",
    "Proxy",
    "RangeError",
    "ReferenceError",
    "Reflect",
    "RegExp",
    "Set",
    "SharedArrayBuffer",
    "String",
    "Symbol",
    "SyntaxError",
    "TypeError",
    "URIError",
    "Uint8Array",
    "Uint8ClampedArray",
    "Uint16Array",
    "Uint32Array",
    "WeakMap",
    "WeakRef",
    "WeakSet",
    "decodeURI",
    "decodeURIComponent",
    "encodeURI",
    "encodeURIComponent",
    "escape",
    "eval",
    "globalThis",
    "isFinite",
    "isNaN",
    "parseFloat",
    "parseInt",
    "undefined",
    "unescape",
    // QuickJS / Web / NSH 运行时全局
    "AbortController",
    "AbortSignal",
    "CustomEvent",
    "Event",
    "EventTarget",
    "Headers",
    "Request",
    "Response",
    "TextDecoder",
    "TextEncoder",
    "URL",
    "URLSearchParams",
    "atob",
    "btoa",
    "cancelAnimationFrame",
    "clearInterval",
    "clearTimeout",
    "console",
    "crypto",
    "fetch",
    "performance",
    "queueMicrotask",
    "requestAnimationFrame",
    "setInterval",
    "setTimeout",
    "structuredClone",
];

/// 只读校验打包后的 ES 模块仅 import 宿主允许的模块，拒绝未知外部依赖（不做任何 AST 改写）。
fn validate_imports(id: &str, code: &str, served: &[String]) -> Result<(), String> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, code, SourceType::mjs()).parse();
    if let Some(error) = parsed.diagnostics.first() {
        return Err(format!("{id}: {error}"));
    }
    for statement in &parsed.program.body {
        let Statement::ImportDeclaration(import) = statement else {
            continue;
        };
        // 按包里剩下的 import 查，不按 `Bundle::natives`：打包时认不得的模块当外部依赖原样留着，
        // 不在那张表里。
        let source = import.source.value.as_str();
        if !served.iter().any(|name| name == source) {
            return Err(format!("{id} 引了宿主没给的模块：{source}"));
        }
    }
    Ok(())
}

/// 静态检查打包后的 ES 模块不含未声明的自由标识符（供 `check_plugin` 与内置插件校验用）。
pub(crate) fn validate_unresolved_references(id: &str, code: &str) -> Result<(), String> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, code, SourceType::mjs()).parse();
    if let Some(error) = parsed.diagnostics.first() {
        return Err(format!("{id}: {error}"));
    }
    let semantic = SemanticBuilder::new().build(&parsed.program).semantic;
    let mut unknown: Vec<String> = semantic
        .scoping()
        .root_unresolved_references()
        .keys()
        .map(|name| name.as_str())
        .filter(|name| !ALLOWED_GLOBALS.contains(name))
        .map(str::to_owned)
        .collect();
    if !unknown.is_empty() {
        unknown.sort();
        return Err(format!("{id} 引用了未定义的标识符：{}", unknown.join(", ")));
    }
    Ok(())
}

/// 从插件入口源码里提取导出的值名称（不含仅类型导出），供 `smelt plugin check` 展示。
pub(crate) fn extract_entry_exports(path: &Path) -> Vec<String> {
    let Ok(source) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let source_type = SourceType::from_path(path).unwrap_or_else(|_| SourceType::tsx());
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, &source, source_type).parse();
    let mut out = BTreeSet::new();
    for statement in &parsed.program.body {
        match statement {
            Statement::ExportDefaultDeclaration(_) => {
                out.insert("default".to_owned());
            }
            Statement::ExportDeclaration(decl) => match &decl.declaration {
                oxc::ast::ast::Declaration::FunctionDeclaration(f) => {
                    if let Some(id) = &f.id {
                        out.insert(id.name.as_str().to_owned());
                    }
                }
                oxc::ast::ast::Declaration::ClassDeclaration(c) => {
                    if let Some(id) = &c.id {
                        out.insert(id.name.as_str().to_owned());
                    }
                }
                oxc::ast::ast::Declaration::VariableDeclaration(vars) => {
                    for d in &vars.declarations {
                        collect_binding_names(&d.id, &mut out);
                    }
                }
                _ => {}
            },
            Statement::ExportNamedDeclaration(decl) => {
                if decl.export_kind.is_type() {
                    continue;
                }
                for specifier in &decl.specifiers {
                    if specifier.export_kind.is_type() {
                        continue;
                    }
                    out.insert(specifier.exported.name().as_str().to_owned());
                }
            }
            Statement::ExportFromDeclaration(decl) => {
                if decl.export_kind.is_type() {
                    continue;
                }
                for specifier in &decl.specifiers {
                    if specifier.export_kind.is_type() {
                        continue;
                    }
                    out.insert(specifier.exported.name().as_str().to_owned());
                }
            }
            _ => {}
        }
    }
    out.into_iter().collect()
}

fn collect_binding_names(pattern: &oxc::ast::ast::BindingPattern<'_>, out: &mut BTreeSet<String>) {
    use oxc::ast::ast::BindingPattern;
    match pattern {
        BindingPattern::BindingIdentifier(id) => {
            out.insert(id.name.as_str().to_owned());
        }
        BindingPattern::ObjectPattern(obj) => {
            for prop in &obj.properties {
                collect_binding_names(&prop.value, out);
            }
            if let Some(rest) = &obj.rest {
                collect_binding_names(&rest.argument, out);
            }
        }
        BindingPattern::ArrayPattern(arr) => {
            for elem in arr.elements.iter().flatten() {
                collect_binding_names(elem, out);
            }
            if let Some(rest) = &arr.rest {
                collect_binding_names(&rest.argument, out);
            }
        }
        BindingPattern::AssignmentPattern(assign) => {
            collect_binding_names(&assign.left, out);
        }
    }
}

pub(crate) fn valid_id(id: &str) -> bool {
    !id.starts_with('.')
        && !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// `file` 在 `dir` 里。两边都是规范过的绝对路径，不含 `..`。
fn inside(file: &Path, dir: &Path) -> bool {
    file.starts_with(dir) && !file.components().any(|part| part == Component::ParentDir)
}

/// 目录里的文件，不跟符号链接，忽略测试与隐藏/预编译条目（与 `manifest::copy_dir_filtered` 一致）。
fn read_dir(root: &Path) -> io::Result<BTreeMap<PathBuf, Vec<u8>>> {
    let mut files = BTreeMap::new();
    let mut dirs = vec![PathBuf::new()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(root.join(&dir))? {
            let entry = entry?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            let kind = entry.file_type()?;
            if is_ignored_plugin_entry(&name_str, kind.is_dir()) {
                continue;
            }
            let path = dir.join(&name);
            if kind.is_dir() {
                dirs.push(path);
            } else if kind.is_file() {
                files.insert(path, fs::read(entry.path())?);
            }
        }
    }
    Ok(files)
}

/// 把 `dir` 整个换成这些文件。
fn write_dir<'a>(
    dir: &Path,
    files: impl IntoIterator<Item = (&'a Path, &'a [u8])>,
) -> io::Result<()> {
    if dir.exists() {
        fs::remove_dir_all(dir)?;
    }
    for (path, bytes) in files {
        let path = dir.join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, bytes)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_plugin_host_disables_wall_clock_cpu_window() {
        let host = PluginHost::new();
        assert_eq!(host.lock().cpu_window_budget, None);
    }

    #[test]
    fn validate_unresolved_references_rejects_undeclared_identifier() {
        let err = validate_unresolved_references(
            "demo",
            "export function f() { return missingHelper(1); }",
        )
        .expect_err("未定义标识符应当被拦截");
        assert!(
            err.contains("missingHelper"),
            "错误信息应点出未定义名：{err}"
        );
    }

    #[test]
    fn all_builtin_plugins_have_no_undeclared_identifiers() {
        let stage =
            std::env::temp_dir().join(format!("smelt-nsh-shell-check-{}", uuid::Uuid::new_v4()));
        let shell_plugins = Plugins::new(
            &stage,
            include_str!("../shell/MainShell.tsx"),
            crate::ui_sets(),
        );
        let _ = fs::remove_dir_all(&stage);
        let shell_plugins = shell_plugins.expect("MainShell.tsx 打包失败");
        validate_unresolved_references(
            SHELL_OWNER,
            shell_plugins
                .shell_source()
                .expect("MainShell.tsx 源码应存在"),
        )
        .expect("MainShell.tsx 存在未定义标识符");

        let repo_plugins = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
        let mut errors = Vec::new();
        for entry in fs::read_dir(&repo_plugins).expect("读取 plugins/") {
            let path = entry.expect("dir entry").path();
            if path.join(crate::MANIFEST_FILE).is_file()
                && let Err(err) = crate::check_plugin(&path)
            {
                errors.push(format!("{}: {err}", path.display()));
            }
        }
        assert!(
            errors.is_empty(),
            "内置插件静态校验失败：\n{}",
            errors.join("\n")
        );
    }

    #[test]
    fn smelt_host_ui_set_matches_build_description() {
        use nsh_core::element::description::ExportKind;
        let sets = crate::ui_sets();
        let host = sets
            .iter()
            .find(|s| s.specifier == "smelt/host")
            .expect("ui_sets 应包含 smelt/host");
        let mut actual: Vec<(&str, &str)> = host
            .exports
            .iter()
            .map(|e| {
                let kind = match e.kind {
                    ExportKind::Component => "Component",
                    ExportKind::Module => "Module",
                    _ => "Other",
                };
                (e.name.as_str(), kind)
            })
            .collect();
        actual.sort_unstable();
        let mut expected = vec![
            ("Icon", "Component"),
            ("Smelt", "Module"),
            ("Vector", "Component"),
        ];
        expected.sort_unstable();
        assert_eq!(actual, expected, "smelt/host 导出需与 build.rs 保持一致");
        assert!(host.reexported_values.is_empty());
        assert!(host.native_values.is_empty());
    }
}
