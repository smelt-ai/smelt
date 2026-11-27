//! 用户插件与覆写态共享内置组合的磁盘 QuickJS 字节码缓存（`~/.smelt/cache/plugins/`）。
//!
//! 只把由当前 Smelt 进程自己调用 `nsh_js::compile_bytecode_with` 产出的字节码落盘到宿主私有缓存目录，
//! 绝不读取用户插件目录内部自带的 `.nshc`；读取前逐项校验格式版本、`nsh_js::BYTECODE_VERSION`、
//! `sdk_fingerprint`、`ui_sets_fingerprint`、源码与共享模块指纹以及字节码文件本身的 SHA-256，
//! 任何不一致、损坏或符号链接一律视为未命中并安全回退到源码重编。

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
};

use nsh_core::element::description::SetDescription;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::plugins::{ModulePackage, valid_id};

const CACHE_FORMAT_VERSION: u32 = 1;
const BUILTINS_STEM: &str = "builtins";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct StandalonePluginMeta {
    format_version: u32,
    bytecode_version: String,
    sdk_fingerprint: String,
    ui_sets_fingerprint: String,
    id: String,
    entry: String,
    requires: Vec<String>,
    source_fingerprint: String,
    used_modules: BTreeMap<String, String>,
    used_module_ids: Vec<String>,
    bytecode_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CachedBuiltinMember {
    pub id: String,
    pub entry: String,
    pub requires: Vec<String>,
    pub source_fingerprint: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub module_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct BuiltinsSubsetMeta {
    format_version: u32,
    bytecode_version: String,
    sdk_fingerprint: String,
    ui_sets_fingerprint: String,
    members: Vec<CachedBuiltinMember>,
    used_modules: BTreeMap<String, String>,
    bytecode_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ValidatedModuleMeta {
    format_version: u32,
    bytecode_version: String,
    sdk_fingerprint: String,
    ui_sets_fingerprint: String,
    id: String,
    specifier: String,
    entry: String,
    requires: Vec<String>,
    module_fingerprint: String,
}

/// 命中单插件缓存后的返回载荷。
pub(crate) struct CachedPluginBundle {
    pub bytecode: Vec<u8>,
    pub used_modules: BTreeMap<String, String>,
    pub used_module_ids: Vec<String>,
}

/// 命中共享内置组合缓存后的返回载荷。
pub(crate) struct CachedBuiltinsBundle {
    pub bytecode: Vec<u8>,
    pub used_modules: BTreeMap<String, String>,
    pub per_plugin_module_ids: BTreeMap<String, Vec<String>>,
}

/// 单插件缓存键输入（所有字段均参与命中比对）。
pub(crate) struct PluginCacheKey<'a> {
    pub id: &'a str,
    pub entry: &'a str,
    pub requires: &'a [String],
    pub source_fingerprint: &'a str,
}

/// 共享内置组合缓存的单个成员期望描述。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BuiltinMemberExpectation {
    pub id: String,
    pub entry: String,
    pub requires: Vec<String>,
    pub source_fingerprint: String,
}

/// 宿主私有插件字节码缓存句柄。
#[derive(Clone, Debug)]
pub(crate) struct BytecodeCache {
    dir: PathBuf,
    sdk_fp: String,
    ui_sets_fp: String,
}

impl BytecodeCache {
    pub(crate) fn new(dir: PathBuf, sdk_fp: String, ui_sets: &[SetDescription]) -> Self {
        Self {
            dir,
            sdk_fp,
            ui_sets_fp: ui_sets_fingerprint(ui_sets),
        }
    }

    pub(crate) fn load_plugin(
        &self,
        key: &PluginCacheKey<'_>,
        active_modules: &BTreeMap<String, ModulePackage>,
    ) -> Option<CachedPluginBundle> {
        if !valid_id(key.id) || !self.cache_dir_usable() {
            return None;
        }
        let stem = plugin_stem(key.id);
        let meta: StandalonePluginMeta = read_json_regular(&self.meta_path(&stem))?;
        if meta.format_version != CACHE_FORMAT_VERSION
            || meta.bytecode_version != nsh_js::BYTECODE_VERSION
            || meta.sdk_fingerprint != self.sdk_fp
            || meta.ui_sets_fingerprint != self.ui_sets_fp
            || meta.id != key.id
            || meta.entry != key.entry
            || meta.requires != key.requires
            || meta.source_fingerprint != key.source_fingerprint
            || !modules_match(&meta.used_modules, active_modules)
        {
            return None;
        }
        let bytecode = read_verified_bytecode(&self.bytecode_path(&stem), &meta.bytecode_sha256)?;
        Some(CachedPluginBundle {
            bytecode,
            used_modules: meta.used_modules,
            used_module_ids: meta.used_module_ids,
        })
    }

    pub(crate) fn store_plugin(
        &self,
        key: &PluginCacheKey<'_>,
        used_modules: &BTreeMap<String, String>,
        used_module_ids: &[String],
        bytecode: &[u8],
    ) {
        if !valid_id(key.id) || bytecode.is_empty() || !self.ensure_dir() {
            return;
        }
        let stem = plugin_stem(key.id);
        let meta = StandalonePluginMeta {
            format_version: CACHE_FORMAT_VERSION,
            bytecode_version: nsh_js::BYTECODE_VERSION.to_owned(),
            sdk_fingerprint: self.sdk_fp.clone(),
            ui_sets_fingerprint: self.ui_sets_fp.clone(),
            id: key.id.to_owned(),
            entry: key.entry.to_owned(),
            requires: key.requires.to_vec(),
            source_fingerprint: key.source_fingerprint.to_owned(),
            used_modules: used_modules.clone(),
            used_module_ids: used_module_ids.to_vec(),
            bytecode_sha256: sha256_hex(bytecode),
        };
        self.write_pair(&stem, bytecode, &meta);
    }

    pub(crate) fn load_builtins(
        &self,
        members: &[BuiltinMemberExpectation],
        active_modules: &BTreeMap<String, ModulePackage>,
    ) -> Option<CachedBuiltinsBundle> {
        if members.is_empty() || !self.cache_dir_usable() {
            return None;
        }
        let meta: BuiltinsSubsetMeta = read_json_regular(&self.meta_path(BUILTINS_STEM))?;
        if meta.format_version != CACHE_FORMAT_VERSION
            || meta.bytecode_version != nsh_js::BYTECODE_VERSION
            || meta.sdk_fingerprint != self.sdk_fp
            || meta.ui_sets_fingerprint != self.ui_sets_fp
            || meta.members.len() != members.len()
            || !modules_match(&meta.used_modules, active_modules)
        {
            return None;
        }
        if !meta
            .members
            .iter()
            .zip(members.iter())
            .all(|(cached, want)| {
                cached.id == want.id
                    && cached.entry == want.entry
                    && cached.requires == want.requires
                    && cached.source_fingerprint == want.source_fingerprint
            })
        {
            return None;
        }
        let bytecode =
            read_verified_bytecode(&self.bytecode_path(BUILTINS_STEM), &meta.bytecode_sha256)?;
        let per_plugin_module_ids = meta
            .members
            .into_iter()
            .map(|m| (m.id, m.module_ids))
            .collect();
        Some(CachedBuiltinsBundle {
            bytecode,
            used_modules: meta.used_modules,
            per_plugin_module_ids,
        })
    }

    pub(crate) fn store_builtins(
        &self,
        members: &[BuiltinMemberExpectation],
        per_plugin_module_ids: &BTreeMap<String, Vec<String>>,
        used_modules: &BTreeMap<String, String>,
        bytecode: &[u8],
    ) {
        if members.is_empty() || bytecode.is_empty() || !self.ensure_dir() {
            return;
        }
        let cached_members = members
            .iter()
            .map(|m| CachedBuiltinMember {
                id: m.id.clone(),
                entry: m.entry.clone(),
                requires: m.requires.clone(),
                source_fingerprint: m.source_fingerprint.clone(),
                module_ids: per_plugin_module_ids
                    .get(&m.id)
                    .cloned()
                    .unwrap_or_default(),
            })
            .collect();
        let meta = BuiltinsSubsetMeta {
            format_version: CACHE_FORMAT_VERSION,
            bytecode_version: nsh_js::BYTECODE_VERSION.to_owned(),
            sdk_fingerprint: self.sdk_fp.clone(),
            ui_sets_fingerprint: self.ui_sets_fp.clone(),
            members: cached_members,
            used_modules: used_modules.clone(),
            bytecode_sha256: sha256_hex(bytecode),
        };
        self.write_pair(BUILTINS_STEM, bytecode, &meta);
    }

    pub(crate) fn is_module_validated(
        &self,
        id: &str,
        specifier: &str,
        entry: &str,
        requires: &[String],
        module_fp: &str,
    ) -> bool {
        if !valid_id(id) || !self.cache_dir_usable() {
            return false;
        }
        let stem = module_stem(id);
        let Some(meta) = read_json_regular::<ValidatedModuleMeta>(&self.meta_path(&stem)) else {
            return false;
        };
        meta.format_version == CACHE_FORMAT_VERSION
            && meta.bytecode_version == nsh_js::BYTECODE_VERSION
            && meta.sdk_fingerprint == self.sdk_fp
            && meta.ui_sets_fingerprint == self.ui_sets_fp
            && meta.id == id
            && meta.specifier == specifier
            && meta.entry == entry
            && meta.requires == requires
            && meta.module_fingerprint == module_fp
    }

    pub(crate) fn store_module_validated(
        &self,
        id: &str,
        specifier: &str,
        entry: &str,
        requires: &[String],
        module_fp: &str,
    ) {
        if !valid_id(id) || !self.ensure_dir() {
            return;
        }
        let stem = module_stem(id);
        let meta = ValidatedModuleMeta {
            format_version: CACHE_FORMAT_VERSION,
            bytecode_version: nsh_js::BYTECODE_VERSION.to_owned(),
            sdk_fingerprint: self.sdk_fp.clone(),
            ui_sets_fingerprint: self.ui_sets_fp.clone(),
            id: id.to_owned(),
            specifier: specifier.to_owned(),
            entry: entry.to_owned(),
            requires: requires.to_vec(),
            module_fingerprint: module_fp.to_owned(),
        };
        let Ok(bytes) = serde_json::to_vec(&meta) else {
            return;
        };
        if let Err(err) = write_atomic(&self.dir, &self.meta_path(&stem), &bytes) {
            eprintln!("[smelt-nsh] 写入模块校验缓存 {id} 失败：{err}");
        }
    }

    /// 清理已卸载插件、已恢复自带预编译包或上次异常中断遗留的缓存文件。
    pub(crate) fn prune(
        &self,
        keep_plugins: &BTreeSet<String>,
        keep_modules: &BTreeSet<String>,
        keep_builtins: bool,
    ) {
        if !self.cache_dir_usable() {
            return;
        }
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let path = entry.path();
            if name.starts_with(".tmp-") {
                let _ = fs::remove_file(&path);
                continue;
            }
            if let Some(id) = name.strip_prefix("plugin-").and_then(|rest| {
                rest.strip_suffix(".nshc")
                    .or_else(|| rest.strip_suffix(".meta.json"))
            }) {
                if !keep_plugins.contains(id) {
                    let _ = fs::remove_file(&path);
                }
                continue;
            }
            if let Some(id) = name
                .strip_prefix("module-")
                .and_then(|rest| rest.strip_suffix(".meta.json"))
            {
                if !keep_modules.contains(id) {
                    let _ = fs::remove_file(&path);
                }
                continue;
            }
            if !keep_builtins
                && (name == format!("{BUILTINS_STEM}.nshc")
                    || name == format!("{BUILTINS_STEM}.meta.json"))
            {
                let _ = fs::remove_file(&path);
            }
        }
    }

    fn bytecode_path(&self, stem: &str) -> PathBuf {
        self.dir.join(format!("{stem}.nshc"))
    }

    fn meta_path(&self, stem: &str) -> PathBuf {
        self.dir.join(format!("{stem}.meta.json"))
    }

    /// 缓存目录若已存在且是符号链接，一律拒用，避免读写落到非宿主控制的位置。
    fn cache_dir_usable(&self) -> bool {
        matches!(fs::symlink_metadata(&self.dir), Ok(meta) if meta.is_dir())
    }

    fn ensure_dir(&self) -> bool {
        match fs::symlink_metadata(&self.dir) {
            Ok(meta) => meta.is_dir(),
            Err(_) => {
                if let Err(err) = fs::create_dir_all(&self.dir) {
                    eprintln!(
                        "[smelt-nsh] 创建插件字节码缓存目录 {} 失败：{err}",
                        self.dir.display()
                    );
                    return false;
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    let _ = fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700));
                }
                self.cache_dir_usable()
            }
        }
    }

    fn write_pair<M: Serialize>(&self, stem: &str, bytecode: &[u8], meta: &M) {
        let Ok(meta_bytes) = serde_json::to_vec(meta) else {
            return;
        };
        // 先落字节码再落元数据：即使中途崩掉，旧 meta 的 sha256 对不上新字节码，下次启动只会安全回退重编。
        if let Err(err) = write_atomic(&self.dir, &self.bytecode_path(stem), bytecode) {
            eprintln!("[smelt-nsh] 写入字节码缓存 {stem} 失败：{err}");
            return;
        }
        if let Err(err) = write_atomic(&self.dir, &self.meta_path(stem), &meta_bytes) {
            eprintln!("[smelt-nsh] 写入字节码缓存元数据 {stem} 失败：{err}");
        }
    }
}

fn plugin_stem(id: &str) -> String {
    format!("plugin-{id}")
}

fn module_stem(id: &str) -> String {
    format!("module-{id}")
}

fn modules_match(
    used_modules: &BTreeMap<String, String>,
    active_modules: &BTreeMap<String, ModulePackage>,
) -> bool {
    used_modules.iter().all(|(spec, fp)| {
        active_modules
            .get(spec)
            .is_some_and(|active| active.fingerprint == *fp)
    })
}

fn read_regular_file(path: &Path) -> Option<Vec<u8>> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    fs::read(path).ok()
}

fn read_json_regular<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    let bytes = read_regular_file(path)?;
    serde_json::from_slice(&bytes).ok()
}

fn read_verified_bytecode(path: &Path, expected_sha256: &str) -> Option<Vec<u8>> {
    let bytes = read_regular_file(path)?;
    if bytes.is_empty() || sha256_hex(&bytes) != expected_sha256 {
        return None;
    }
    Some(bytes)
}

fn write_atomic(dir: &Path, target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = dir.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let write_res = (|| -> std::io::Result<()> {
        let mut file = opts.open(&tmp)?;
        file.write_all(bytes)?;
        file.flush()?;
        Ok(())
    })();
    if let Err(err) = write_res {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    if let Err(err) = fs::rename(&tmp, target) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

/// 为当前原生组件集生成确定性指纹：`compile_bytecode_with` 依赖 `ui_sets` 的模块表，
/// 任何原生导出或资源变动都必须使旧字节码缓存失效。
pub(crate) fn ui_sets_fingerprint(ui_sets: &[SetDescription]) -> String {
    let mut hasher = Sha256::new();
    for set in ui_sets {
        hasher.update(set.specifier.as_bytes());
        hasher.update([0]);
        for export in &set.exports {
            hasher.update(export.name.as_bytes());
            hasher.update([0]);
            hasher.update(export.kind.as_str().as_bytes());
            hasher.update([0]);
        }
        hasher.update([1]);
        hasher.update(set.declarations.as_bytes());
        hasher.update([2]);
        for name in &set.reexported_values {
            hasher.update(name.as_bytes());
            hasher.update([0]);
        }
        hasher.update([3]);
        for name in &set.native_values {
            hasher.update(name.as_bytes());
            hasher.update([0]);
        }
        hasher.update([4]);
        for asset in &set.assets {
            hasher.update(asset.path.as_bytes());
            hasher.update([0]);
            hasher.update(asset.source.as_bytes());
            hasher.update([0]);
        }
        hasher.update([5]);
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(16);
    for byte in &digest[..8] {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "smelt-bytecode-cache-{name}-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fake_module(id: &str, spec: &str, fp: &str) -> (String, ModulePackage) {
        (
            spec.to_owned(),
            ModulePackage {
                id: id.to_owned(),
                specifier: spec.to_owned(),
                requires: Vec::new(),
                root: PathBuf::from("/workspace/mod"),
                entry: "index.tsx".to_owned(),
                types: None,
                fingerprint: fp.to_owned(),
                source: crate::PluginSource::Builtin,
            },
        )
    }

    #[test]
    fn standalone_plugin_cache_round_trips_and_invalidates_on_any_change() {
        let dir = Scratch::new("standalone");
        let ui_sets = crate::ui_sets();
        let cache = BytecodeCache::new(dir.0.join("cache"), "sdk-fp-1".to_owned(), &ui_sets);
        let modules: BTreeMap<String, ModulePackage> =
            [fake_module("smelt.avatar", "smelt/avatar", "mod-fp-a")].into();
        let used_modules: BTreeMap<String, String> =
            [("smelt/avatar".to_owned(), "mod-fp-a".to_owned())].into();
        let req = vec!["smelt/avatar".to_owned()];
        let key = PluginCacheKey {
            id: "com.example.demo",
            entry: "nsh.tsx",
            requires: &req,
            source_fingerprint: "src-fp-1",
        };

        assert!(cache.load_plugin(&key, &modules).is_none());
        cache.store_plugin(&key, &used_modules, &["smelt.avatar".to_owned()], b"BC01");

        let hit = cache.load_plugin(&key, &modules).expect("应命中缓存");
        assert_eq!(hit.bytecode, b"BC01");
        assert_eq!(hit.used_modules, used_modules);
        assert_eq!(hit.used_module_ids, ["smelt.avatar"]);

        // 源码指纹变动 -> 失效
        let changed_src = PluginCacheKey {
            source_fingerprint: "src-fp-2",
            ..key
        };
        assert!(cache.load_plugin(&changed_src, &modules).is_none());

        // 依赖共享模块指纹变动 -> 失效
        let updated_modules: BTreeMap<String, ModulePackage> =
            [fake_module("smelt.avatar", "smelt/avatar", "mod-fp-b")].into();
        assert!(cache.load_plugin(&key, &updated_modules).is_none());

        // SDK 指纹变动 -> 失效
        let other_sdk = BytecodeCache::new(dir.0.join("cache"), "sdk-fp-2".to_owned(), &ui_sets);
        assert!(other_sdk.load_plugin(&key, &modules).is_none());

        // 字节码文件损坏/被篡改 -> SHA-256 拦截，安全未命中
        fs::write(
            dir.0.join("cache").join("plugin-com.example.demo.nshc"),
            b"TAMPERED",
        )
        .unwrap();
        assert!(cache.load_plugin(&key, &modules).is_none());
    }

    #[test]
    fn builtins_subset_cache_round_trips_and_prunes_stale_entries() {
        let dir = Scratch::new("builtins");
        let ui_sets = crate::ui_sets();
        let cache = BytecodeCache::new(dir.0.join("cache"), "sdk-fp-1".to_owned(), &ui_sets);
        let modules: BTreeMap<String, ModulePackage> = BTreeMap::new();
        let members = vec![
            BuiltinMemberExpectation {
                id: "com.example.a".to_owned(),
                entry: "a.tsx".to_owned(),
                requires: Vec::new(),
                source_fingerprint: "fp-a".to_owned(),
            },
            BuiltinMemberExpectation {
                id: "com.example.b".to_owned(),
                entry: "b.tsx".to_owned(),
                requires: Vec::new(),
                source_fingerprint: "fp-b".to_owned(),
            },
        ];
        let per_plugin_mod_ids: BTreeMap<String, Vec<String>> = [
            ("com.example.a".to_owned(), Vec::new()),
            ("com.example.b".to_owned(), vec!["smelt.avatar".to_owned()]),
        ]
        .into();

        cache.store_builtins(&members, &per_plugin_mod_ids, &BTreeMap::new(), b"BC_ALL");
        let hit = cache
            .load_builtins(&members, &modules)
            .expect("应命中内置组合缓存");
        assert_eq!(hit.bytecode, b"BC_ALL");
        assert_eq!(hit.per_plugin_module_ids["com.example.b"], ["smelt.avatar"]);

        // 成员列表变化 -> 失效
        assert!(cache.load_builtins(&members[..1], &modules).is_none());

        // 写入待清理的旧插件、旧模块与临时文件，prune 应只删不在活跃集合里的条目。
        let keep_key = PluginCacheKey {
            id: "com.example.keep",
            entry: "main.tsx",
            requires: &[],
            source_fingerprint: "fp-k",
        };
        let drop_key = PluginCacheKey {
            id: "com.example.drop",
            entry: "main.tsx",
            requires: &[],
            source_fingerprint: "fp-d",
        };
        cache.store_plugin(&keep_key, &BTreeMap::new(), &[], b"KEEP");
        cache.store_plugin(&drop_key, &BTreeMap::new(), &[], b"DROP");
        cache.store_module_validated("com.example.mod", "custom/mod", "index.ts", &[], "mfp");
        fs::write(dir.0.join("cache").join(".tmp-stale"), b"tmp").unwrap();

        let keep_plugins: BTreeSet<String> = ["com.example.keep".to_owned()].into();
        let keep_modules: BTreeSet<String> = BTreeSet::new();
        cache.prune(&keep_plugins, &keep_modules, false);

        assert!(dir.0.join("cache/plugin-com.example.keep.nshc").is_file());
        assert!(!dir.0.join("cache/plugin-com.example.drop.nshc").exists());
        assert!(
            !dir.0
                .join("cache/module-com.example.mod.meta.json")
                .exists()
        );
        assert!(!dir.0.join("cache/builtins.nshc").exists());
        assert!(!dir.0.join("cache/.tmp-stale").exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_cache_files_are_refused() {
        let dir = Scratch::new("symlink");
        let ui_sets = crate::ui_sets();
        let cache_dir = dir.0.join("cache");
        let cache = BytecodeCache::new(cache_dir.clone(), "sdk-fp-1".to_owned(), &ui_sets);
        let modules = BTreeMap::new();
        let key = PluginCacheKey {
            id: "com.example.sym",
            entry: "main.tsx",
            requires: &[],
            source_fingerprint: "fp-s",
        };
        cache.store_plugin(&key, &BTreeMap::new(), &[], b"REAL");
        let real = dir.0.join("outside.nshc");
        fs::write(&real, b"REAL").unwrap();
        let target = cache_dir.join("plugin-com.example.sym.nshc");
        fs::remove_file(&target).unwrap();
        std::os::unix::fs::symlink(&real, &target).unwrap();

        assert!(
            cache.load_plugin(&key, &modules).is_none(),
            "符号链接缓存文件必须拒绝读取"
        );
    }
}
