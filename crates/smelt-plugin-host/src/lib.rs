//! Discovery, authentication and process supervision for bundled Smelt plugins.

#![cfg(unix)]

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use smelt_plugin_api::{
    Capability, Contribution, InvocationRequest, PLUGIN_AGENT_MANIFEST_FILE,
    PLUGIN_INPUT_MANIFEST_FILE, PLUGIN_UI_MANIFEST_FILE, PluginAgentManifest, PluginId,
    PluginInputManifest, PluginManifest, PluginUiManifest,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    fs::{self, File},
    io::{self, Read, Write},
    os::{fd::OwnedFd, unix::fs::PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};

mod shared_bun;

pub use shared_bun::{SharedBunHost, SharedBunHostOptions};

const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const PLUGIN_RUNTIME_DIR: &str = "runtime/plugins";
const PLUGIN_SETS_DIR: &str = "sets";
const DAEMON_SETS_DIR: &str = "daemon-sets";
const COMPLETE_MARKER: &str = ".complete";
const USER_PLUGIN_DIR: &str = "plugins";
const USER_PLUGIN_REGISTRY: &str = ".installed.json";
static USER_PLUGIN_STORE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostError(String);

impl HostError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HostError {}

impl From<io::Error> for HostError {
    fn from(error: io::Error) -> Self {
        Self::new(error.to_string())
    }
}

#[derive(Clone, Debug)]
pub struct PluginPackage {
    root: PathBuf,
    entrypoint: PathBuf,
    manifest: PluginManifest,
    provenance: PluginProvenance,
}

/// 包来自哪里由宿主赋值，不能由 `plugin.json` 自己声明。
///
/// `UserInstalled.digest` 是安装时记下的整包摘要。每次发现都会重算并比对，因此用户
/// 目录里的包被原地改过后不会继续启动。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginProvenance {
    FirstParty,
    UserInstalled { digest: String },
}

impl PluginPackage {
    pub fn load(root: impl AsRef<Path>) -> Result<Self, HostError> {
        Self::load_with_provenance(root, PluginProvenance::FirstParty)
    }

    fn load_with_provenance(
        root: impl AsRef<Path>,
        provenance: PluginProvenance,
    ) -> Result<Self, HostError> {
        let root = root
            .as_ref()
            .canonicalize()
            .map_err(|error| HostError::new(format!("canonicalize plugin package: {error}")))?;
        if !root.is_dir() {
            return Err(HostError::new("plugin package is not a directory"));
        }
        let manifest_path = root.join("plugin.json");
        let metadata = fs::symlink_metadata(&manifest_path)
            .map_err(|error| HostError::new(format!("read plugin manifest metadata: {error}")))?;
        if !metadata.is_file() || metadata.len() > MAX_MANIFEST_BYTES {
            return Err(HostError::new(
                "plugin manifest is not a bounded regular file",
            ));
        }
        let bytes = fs::read(&manifest_path)
            .map_err(|error| HostError::new(format!("read plugin manifest: {error}")))?;
        let mut manifest = serde_json::from_slice::<PluginManifest>(&bytes)
            .map_err(|error| HostError::new(format!("decode plugin manifest: {error}")))?;
        if manifest
            .contributions
            .iter()
            .any(|contribution| !matches!(contribution, Contribution::Command { .. }))
        {
            return Err(HostError::new(format!(
                "base plugin manifest may only contain command contributions; move UI contributions to {PLUGIN_UI_MANIFEST_FILE}, input routes to {PLUGIN_INPUT_MANIFEST_FILE}, and agents to {PLUGIN_AGENT_MANIFEST_FILE}"
            )));
        }
        manifest
            .validate()
            .map_err(|error| HostError::new(format!("validate plugin manifest: {error}")))?;
        merge_optional_ui_manifest(&root, &mut manifest);
        merge_optional_input_manifest(&root, &mut manifest);
        merge_optional_agent_manifest(&root, &mut manifest);

        let declared_entrypoint = root.join(&manifest.entrypoint);
        let declared_metadata = fs::symlink_metadata(&declared_entrypoint)
            .map_err(|error| HostError::new(format!("read plugin entrypoint metadata: {error}")))?;
        if declared_metadata.file_type().is_symlink() {
            return Err(HostError::new("plugin entrypoint cannot be a symlink"));
        }
        let entrypoint = declared_entrypoint
            .canonicalize()
            .map_err(|error| HostError::new(format!("canonicalize plugin entrypoint: {error}")))?;
        if !entrypoint.starts_with(&root) {
            return Err(HostError::new("plugin entrypoint escapes its package"));
        }
        let entrypoint_metadata = fs::metadata(&entrypoint)
            .map_err(|error| HostError::new(format!("read plugin entrypoint metadata: {error}")))?;
        if !entrypoint_metadata.is_file() {
            return Err(HostError::new("plugin entrypoint is not a regular file"));
        }
        Ok(Self {
            root,
            entrypoint,
            manifest,
            provenance,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn entrypoint(&self) -> &Path {
        &self.entrypoint
    }

    /// 检查当前机器是否具备启动该 package 所需的受管 Bun。GUI 只需要这个判断。
    pub fn runtime_available(&self, bun: Option<&Path>) -> Result<(), HostError> {
        match bun {
            Some(path) if path.is_file() => Ok(()),
            _ => Err(HostError::new("plugin runtime bun is unavailable")),
        }
    }

    pub fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    pub fn provenance(&self) -> &PluginProvenance {
        &self.provenance
    }

    /// Recheck the installation digest before a host reads module data again.
    ///
    /// First-party package sets are bound to the daemon build. User-installed packages carry
    /// their approved digest and must not be reloaded after in-place modification.
    pub fn verify_integrity(&self) -> Result<(), HostError> {
        let PluginProvenance::UserInstalled { digest } = &self.provenance else {
            return Ok(());
        };
        if package_fingerprint(self)? != *digest {
            return Err(HostError::new(format!(
                "installed plugin {} changed after installation",
                self.manifest.id
            )));
        }
        Ok(())
    }

    pub fn validate_invocation(&self, request: &InvocationRequest) -> Result<(), HostError> {
        let declared = self
            .manifest
            .contributions
            .iter()
            .find(|contribution| contribution.id() == &request.contribution_id)
            .ok_or_else(|| HostError::new("plugin contribution is not declared"))?;
        if contribution_accepts_operation(declared, &request.operation) {
            Ok(())
        } else {
            Err(HostError::new(
                "invocation operation does not match the declared contribution",
            ))
        }
    }

    /// 解析包内资源的真实路径。
    ///
    /// manifest 是插件自己写的数据，不能当安全依据，所以这里不信任 `relative`
    /// 的形状，一律 canonicalize 之后确认仍落在包内——符号链接指向包外也会被
    /// 这一步挡住。
    pub fn resolve_asset(&self, relative: &str) -> Option<PathBuf> {
        let candidate = self.root.join(relative).canonicalize().ok()?;
        candidate.starts_with(&self.root).then_some(candidate)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledUserPlugin {
    pub id: String,
    pub name: String,
    pub version: String,
    pub digest: String,
    pub approved_capabilities: Vec<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum StoredUserPlugin {
    /// P2 开发版写过的旧形态。没有授权记录，不能据它授予任何 capability。
    LegacyDigest(String),
    Approved {
        digest: String,
        #[serde(default)]
        capabilities: Vec<Capability>,
    },
}

impl StoredUserPlugin {
    fn digest(&self) -> &str {
        match self {
            Self::LegacyDigest(digest) | Self::Approved { digest, .. } => digest,
        }
    }

    fn approved_capabilities(&self) -> Option<&[Capability]> {
        match self {
            Self::LegacyDigest(_) => None,
            Self::Approved { capabilities, .. } => Some(capabilities),
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UserPluginRegistry {
    #[serde(default)]
    plugins: BTreeMap<String, StoredUserPlugin>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginInstallPlan {
    pub source: PathBuf,
    pub id: String,
    pub name: String,
    pub version: String,
    pub digest: String,
    pub previous_version: Option<String>,
    pub requested_capabilities: Vec<String>,
    pub added_capabilities: Vec<String>,
    pub removed_capabilities: Vec<String>,
    /// 首装即使零 capability 也要确认，因为确认的还有“运行这份第三方代码”。
    pub requires_confirmation: bool,
    previous_record: Option<StoredUserPlugin>,
}

pub fn user_plugin_root(smelt_root: &Path) -> PathBuf {
    smelt_root.join(USER_PLUGIN_DIR)
}

fn user_plugin_store_lock() -> std::sync::MutexGuard<'static, ()> {
    USER_PLUGIN_STORE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

fn user_plugin_registry_path(smelt_root: &Path) -> PathBuf {
    user_plugin_root(smelt_root).join(USER_PLUGIN_REGISTRY)
}

fn load_user_plugin_registry(smelt_root: &Path) -> Result<UserPluginRegistry, HostError> {
    let path = user_plugin_registry_path(smelt_root);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(UserPluginRegistry::default());
        }
        Err(error) => {
            return Err(HostError::new(format!(
                "read user plugin registry {}: {error}",
                path.display()
            )));
        }
    };
    serde_json::from_slice(&bytes).map_err(|error| {
        HostError::new(format!(
            "decode user plugin registry {}: {error}",
            path.display()
        ))
    })
}

fn save_user_plugin_registry(
    smelt_root: &Path,
    registry: &UserPluginRegistry,
) -> Result<(), HostError> {
    let root = user_plugin_root(smelt_root);
    fs::create_dir_all(&root)?;
    let bytes = serde_json::to_vec_pretty(registry)
        .map_err(|error| HostError::new(format!("encode user plugin registry: {error}")))?;
    atomic_write(&user_plugin_registry_path(smelt_root), &bytes)
}

/// 发现通过安装 API 落到 `~/.smelt/plugins` 的包。
///
/// registry 没登记的目录一律不认；登记过但内容摘要不匹配则返回错误。这样来源判断
/// 不依赖 manifest 自称，也不会把用户随手复制进去的目录当成已安装插件。
pub fn discover_user_plugins(smelt_root: &Path) -> Vec<Result<PluginPackage, HostError>> {
    let _guard = user_plugin_store_lock();
    discover_user_plugins_unlocked(smelt_root)
}

fn discover_user_plugins_unlocked(smelt_root: &Path) -> Vec<Result<PluginPackage, HostError>> {
    let registry = match load_user_plugin_registry(smelt_root) {
        Ok(registry) => registry,
        Err(error) => return vec![Err(error)],
    };
    registry
        .plugins
        .into_iter()
        .map(|(id, record)| load_registered_user_plugin(smelt_root, &id, &record))
        .collect()
}

fn load_registered_user_plugin(
    smelt_root: &Path,
    id: &str,
    record: &StoredUserPlugin,
) -> Result<PluginPackage, HostError> {
    let expected_digest = record.digest();
    if !is_sha256_hex(expected_digest) {
        return Err(HostError::new(format!(
            "installed digest for plugin {id} is invalid"
        )));
    }
    let root = user_plugin_root(smelt_root).join(id);
    let package = PluginPackage::load_with_provenance(
        &root,
        PluginProvenance::UserInstalled {
            digest: expected_digest.to_string(),
        },
    )?;
    if package.manifest().id.as_str() != id {
        return Err(HostError::new(format!(
            "installed plugin directory {id} contains manifest id {}",
            package.manifest().id
        )));
    }
    package.verify_integrity()?;
    let Some(approved) = record.approved_capabilities() else {
        return Err(HostError::new(format!(
            "installed plugin {id} requires capability approval"
        )));
    };
    let requested = package
        .manifest()
        .capabilities
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let approved = approved.iter().cloned().collect::<BTreeSet<_>>();
    if requested != approved {
        return Err(HostError::new(format!(
            "installed plugin {id} capabilities differ from the approved set"
        )));
    }
    Ok(package)
}

/// 合并应用自带与用户安装的包。自带包先占 ID；用户包不能覆盖它，也不能彼此重名。
///
/// 重名只拒绝冲突的用户包，不拖垮整个 supervisor——否则往用户目录放一个
/// `com.smelt.skills` 就能让所有第一方插件一起消失。
pub fn discover_all_plugins(
    bundled_root: Option<&Path>,
    smelt_root: &Path,
) -> Vec<Result<PluginPackage, HostError>> {
    let mut results = Vec::new();
    let mut seen = BTreeSet::new();
    if let Some(root) = bundled_root {
        for result in discover_packages(root) {
            if let Ok(package) = &result {
                seen.insert(package.manifest().id.clone());
            }
            results.push(result);
        }
    }
    for result in discover_user_plugins(smelt_root) {
        match result {
            Ok(package) if seen.insert(package.manifest().id.clone()) => {
                results.push(Ok(package));
            }
            Ok(package) => results.push(Err(HostError::new(format!(
                "user plugin {} conflicts with an already discovered plugin",
                package.manifest().id
            )))),
            Err(error) => results.push(Err(error)),
        }
    }
    results
}

pub fn list_user_plugins(smelt_root: &Path) -> Vec<Result<InstalledUserPlugin, HostError>> {
    let _guard = user_plugin_store_lock();
    let registry = match load_user_plugin_registry(smelt_root) {
        Ok(registry) => registry,
        Err(error) => return vec![Err(error)],
    };
    registry
        .plugins
        .into_iter()
        .map(|(id, stored)| {
            let digest = stored.digest().to_string();
            let approved_capabilities = stored
                .approved_capabilities()
                .unwrap_or_default()
                .iter()
                .map(|capability| capability.as_str().to_string())
                .collect();
            let record = match load_registered_user_plugin(smelt_root, &id, &stored) {
                Ok(package) => InstalledUserPlugin {
                    id,
                    name: package.manifest().name.clone(),
                    version: package.manifest().version.clone(),
                    digest,
                    approved_capabilities,
                    error: None,
                },
                Err(error) => InstalledUserPlugin {
                    name: id.clone(),
                    version: "不可用".to_string(),
                    id,
                    digest,
                    approved_capabilities,
                    error: Some(error.to_string()),
                },
            };
            Ok(record)
        })
        .collect()
}

/// 读取候选包并计算相对上次批准权限的 diff。此函数不改磁盘。
pub fn plan_user_plugin_install(
    smelt_root: &Path,
    source: &Path,
) -> Result<PluginInstallPlan, HostError> {
    let _guard = user_plugin_store_lock();
    let source = PluginPackage::load(source)?;
    let digest = package_fingerprint(&source)?;
    let id = source.manifest().id.as_str().to_string();
    let registry = load_user_plugin_registry(smelt_root)?;
    let previous = registry.plugins.get(&id);
    let previous_record = previous.cloned();
    let approved = previous
        .and_then(StoredUserPlugin::approved_capabilities)
        .unwrap_or_default()
        .iter()
        .map(Capability::as_str)
        .collect::<BTreeSet<_>>();
    let requested = source
        .manifest()
        .capabilities
        .iter()
        .map(Capability::as_str)
        .collect::<BTreeSet<_>>();
    let added_capabilities = requested
        .difference(&approved)
        .map(|value| (*value).to_string())
        .collect::<Vec<_>>();
    let removed_capabilities = approved
        .difference(&requested)
        .map(|value| (*value).to_string())
        .collect::<Vec<_>>();
    let previous_version = previous.and_then(|record| {
        let package = PluginPackage::load(user_plugin_root(smelt_root).join(&id)).ok()?;
        (package_fingerprint(&package).ok()?.as_str() == record.digest())
            .then(|| package.manifest().version.clone())
    });
    let requires_confirmation = previous.is_none()
        || previous.is_some_and(|record| record.approved_capabilities().is_none())
        || !added_capabilities.is_empty();
    Ok(PluginInstallPlan {
        source: source.root().to_path_buf(),
        id,
        name: source.manifest().name.clone(),
        version: source.manifest().version.clone(),
        digest,
        previous_version,
        requested_capabilities: requested.into_iter().map(str::to_string).collect(),
        added_capabilities,
        removed_capabilities,
        requires_confirmation,
        previous_record,
    })
}

/// 提交一份已经由用户确认过的安装计划。
///
/// 用户安装的包和第一方一样走受管 Bun。Bun 不是 sandbox：安装确认必须把
/// 「以当前用户身份跑」说清楚，capability 清单只覆盖宿主代办的 Smelt API。
pub fn install_user_plugin(
    smelt_root: &Path,
    plan: &PluginInstallPlan,
) -> Result<InstalledUserPlugin, HostError> {
    let _guard = user_plugin_store_lock();
    let source = PluginPackage::load(&plan.source)?;
    let digest = package_fingerprint(&source)?;
    let current_registry = load_user_plugin_registry(smelt_root)?;
    if current_registry.plugins.get(&plan.id) != plan.previous_record.as_ref() {
        return Err(HostError::new(
            "installed plugin changed after the installation review",
        ));
    }
    if digest != plan.digest
        || source.manifest().id.as_str() != plan.id
        || source.manifest().version != plan.version
    {
        return Err(HostError::new(
            "plugin package changed after the installation review",
        ));
    }
    let requested_capabilities = source
        .manifest()
        .capabilities
        .iter()
        .map(Capability::as_str)
        .collect::<BTreeSet<_>>();
    let reviewed_capabilities = plan
        .requested_capabilities
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if requested_capabilities != reviewed_capabilities {
        return Err(HostError::new(
            "plugin capabilities changed after the installation review",
        ));
    }
    let id = source.manifest().id.as_str().to_string();
    let root = user_plugin_root(smelt_root);
    fs::create_dir_all(&root)?;
    let destination = root.join(&id);
    let staging = root.join(format!(".staging-{}", uuid::Uuid::new_v4().simple()));
    let retired = root.join(format!(".retired-{}", uuid::Uuid::new_v4().simple()));

    let result = (|| {
        copy_package(source.root(), &staging)?;
        let staged = PluginPackage::load(&staging)?;
        if package_fingerprint(&staged)? != digest {
            return Err(HostError::new("staged plugin digest does not match source"));
        }

        let had_previous = destination.exists();
        if had_previous {
            fs::rename(&destination, &retired)?;
        }
        if let Err(error) = fs::rename(&staging, &destination) {
            if had_previous {
                let _ = fs::rename(&retired, &destination);
            }
            return Err(error.into());
        }

        let mut registry = match load_user_plugin_registry(smelt_root) {
            Ok(registry) => registry,
            Err(error) => {
                let _ = fs::remove_dir_all(&destination);
                if had_previous {
                    let _ = fs::rename(&retired, &destination);
                }
                return Err(error);
            }
        };
        registry.plugins.insert(
            id.clone(),
            StoredUserPlugin::Approved {
                digest: digest.clone(),
                capabilities: source.manifest().capabilities.clone(),
            },
        );
        if let Err(error) = save_user_plugin_registry(smelt_root, &registry) {
            let _ = fs::remove_dir_all(&destination);
            if had_previous {
                let _ = fs::rename(&retired, &destination);
            }
            return Err(error);
        }
        if had_previous {
            let _ = fs::remove_dir_all(&retired);
        }
        Ok(InstalledUserPlugin {
            id,
            name: source.manifest().name.clone(),
            version: source.manifest().version.clone(),
            digest,
            approved_capabilities: plan.requested_capabilities.clone(),
            error: None,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

pub fn uninstall_user_plugin(smelt_root: &Path, plugin_id: &str) -> Result<(), HostError> {
    let _guard = user_plugin_store_lock();
    let plugin_id = PluginId::new(plugin_id).map_err(|error| HostError::new(error.to_string()))?;
    let mut registry = load_user_plugin_registry(smelt_root)?;
    if !registry.plugins.contains_key(plugin_id.as_str()) {
        return Err(HostError::new(format!(
            "user plugin {} is not installed",
            plugin_id
        )));
    }
    let root = user_plugin_root(smelt_root);
    let destination = root.join(plugin_id.as_str());
    let retired = root.join(format!(".retired-{}", uuid::Uuid::new_v4().simple()));
    let retired_package = if destination.exists() {
        fs::rename(&destination, &retired).map_err(|error| {
            HostError::new(format!(
                "retire installed plugin {}: {error}",
                destination.display()
            ))
        })?;
        true
    } else {
        // registry 是安装事实的真源。包目录已被用户手工删掉时仍要允许清掉记录，
        // 否则设置页会留下一个永远卸不掉的幽灵插件。
        false
    };
    registry.plugins.remove(plugin_id.as_str());
    if let Err(error) = save_user_plugin_registry(smelt_root, &registry) {
        if retired_package {
            let _ = fs::rename(&retired, &destination);
        }
        return Err(error);
    }
    if retired_package && let Err(error) = fs::remove_dir_all(&retired) {
        // registry 已提交，语义上插件已经卸载；把清理失败说成卸载失败会让 GUI
        // 跳过 daemon reload，而用户重试又只会得到“未安装”。隐藏的 retired
        // 目录不会被发现，记录日志留给维护页清理。
        eprintln!(
            "[plugin-host] user plugin uninstalled but retired package cleanup failed {}: {error}",
            retired.display()
        );
    }
    Ok(())
}

/// UI sidecar 不能影响插件主体的发现和升级。文件不可读、格式损坏、单条声明
/// 非法或类型未知时都只跳过对应 UI；已知且通过完整 manifest 校验的声明才合并。
fn merge_optional_ui_manifest(root: &Path, manifest: &mut PluginManifest) {
    let path = root.join(PLUGIN_UI_MANIFEST_FILE);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return,
        Err(error) => {
            eprintln!("[plugin-host] read UI manifest {}: {error}", path.display());
            return;
        }
    };
    if !metadata.is_file() || metadata.len() > MAX_MANIFEST_BYTES {
        eprintln!(
            "[plugin-host] ignored unbounded UI manifest {}",
            path.display()
        );
        return;
    }
    let ui_manifest = match fs::read(&path).map_err(HostError::from).and_then(|bytes| {
        serde_json::from_slice::<PluginUiManifest>(&bytes)
            .map_err(|error| HostError::new(format!("decode plugin UI manifest: {error}")))
    }) {
        Ok(ui_manifest) => ui_manifest,
        Err(error) => {
            eprintln!(
                "[plugin-host] ignored UI manifest {}: {error}",
                path.display()
            );
            return;
        }
    };

    // SidebarAccount depends on a SettingsSection and sidecar declaration order is not a
    // protocol. Merge independent/section entries first, then account projections.
    let (accounts, surfaces): (Vec<_>, Vec<_>) = ui_manifest
        .known_contributions()
        .partition(|contribution| matches!(contribution, Contribution::SidebarAccount { .. }));
    for contribution in surfaces.into_iter().chain(accounts) {
        let mut candidate = manifest.clone();
        candidate.contributions.push(contribution);
        match candidate.validate() {
            Ok(()) => *manifest = candidate,
            Err(error) => eprintln!(
                "[plugin-host] ignored invalid UI contribution in {}: {error}",
                path.display()
            ),
        }
    }
}

/// 输入路由 sidecar 同样不能阻断插件主体。文件缺失、损坏或单条声明非法时只跳过
/// 对应路由；旧安装器根本不会打开这个文件。
fn merge_optional_input_manifest(root: &Path, manifest: &mut PluginManifest) {
    let path = root.join(PLUGIN_INPUT_MANIFEST_FILE);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return,
        Err(error) => {
            eprintln!(
                "[plugin-host] read input manifest {}: {error}",
                path.display()
            );
            return;
        }
    };
    if !metadata.is_file() || metadata.len() > MAX_MANIFEST_BYTES {
        eprintln!(
            "[plugin-host] ignored unbounded input manifest {}",
            path.display()
        );
        return;
    }
    let input_manifest = match fs::read(&path).map_err(HostError::from).and_then(|bytes| {
        serde_json::from_slice::<PluginInputManifest>(&bytes)
            .map_err(|error| HostError::new(format!("decode plugin input manifest: {error}")))
    }) {
        Ok(input_manifest) => input_manifest,
        Err(error) => {
            eprintln!(
                "[plugin-host] ignored input manifest {}: {error}",
                path.display()
            );
            return;
        }
    };

    for contribution in input_manifest.known_contributions() {
        let mut candidate = manifest.clone();
        candidate.contributions.push(contribution);
        match candidate.validate() {
            Ok(()) => *manifest = candidate,
            Err(error) => eprintln!(
                "[plugin-host] ignored invalid input contribution in {}: {error}",
                path.display()
            ),
        }
    }
}

/// 智能体声明在输入路由之后合并：controller 可以引用同包的 InputRoute，agent
/// 再引用 controller。按依赖顺序逐条校验，单个坏声明不会拖垮插件主体，也不会
/// 让 sidecar 的书写顺序成为隐式协议。
fn merge_optional_agent_manifest(root: &Path, manifest: &mut PluginManifest) {
    let path = root.join(PLUGIN_AGENT_MANIFEST_FILE);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return,
        Err(error) => {
            eprintln!(
                "[plugin-host] read agent manifest {}: {error}",
                path.display()
            );
            return;
        }
    };
    if !metadata.is_file() || metadata.len() > MAX_MANIFEST_BYTES {
        eprintln!(
            "[plugin-host] ignored unbounded agent manifest {}",
            path.display()
        );
        return;
    }
    let agent_manifest = match fs::read(&path).map_err(HostError::from).and_then(|bytes| {
        serde_json::from_slice::<PluginAgentManifest>(&bytes)
            .map_err(|error| HostError::new(format!("decode plugin agent manifest: {error}")))
    }) {
        Ok(agent_manifest) => agent_manifest,
        Err(error) => {
            eprintln!(
                "[plugin-host] ignored agent manifest {}: {error}",
                path.display()
            );
            return;
        }
    };

    let (controllers, agents): (Vec<_>, Vec<_>) = agent_manifest
        .known_contributions()
        .partition(|contribution| matches!(contribution, Contribution::SessionController { .. }));
    for contribution in controllers.into_iter().chain(agents) {
        let mut candidate = manifest.clone();
        candidate.contributions.push(contribution);
        match candidate.validate() {
            Ok(()) => *manifest = candidate,
            Err(error) => eprintln!(
                "[plugin-host] ignored invalid agent contribution in {}: {error}",
                path.display()
            ),
        }
    }
}

/// 递归拷贝一棵资源树。符号链接一律拒绝：包内容必须是自解释的普通文件，
/// 否则"包边界"在运行期就形同虚设。
fn copy_asset_tree(source: &Path, dest: &Path) -> Result<(), HostError> {
    let metadata = fs::symlink_metadata(source).map_err(|error| {
        HostError::new(format!("read plugin asset {}: {error}", source.display()))
    })?;
    if metadata.file_type().is_symlink() {
        return Err(HostError::new(format!(
            "plugin asset cannot be a symlink: {}",
            source.display()
        )));
    }
    if metadata.is_file() {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, dest)?;
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(HostError::new(format!(
            "plugin asset is neither file nor directory: {}",
            source.display()
        )));
    }
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        copy_asset_tree(&entry.path(), &dest.join(entry.file_name()))?;
    }
    Ok(())
}

pub fn discover_packages(root: impl AsRef<Path>) -> Vec<Result<PluginPackage, HostError>> {
    let root = root.as_ref();
    if !root.exists() {
        return Vec::new();
    }
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) => {
            return vec![Err(HostError::new(format!(
                "read plugin root {}: {error}",
                root.display()
            )))];
        }
    };
    let mut entries = entries.collect::<Vec<_>>();
    entries.sort_by_key(|entry| {
        entry
            .as_ref()
            .ok()
            .map(|entry| entry.file_name())
            .unwrap_or_default()
    });
    entries
        .into_iter()
        .filter_map(|entry| match entry {
            Err(error) => Some(Err(HostError::new(format!(
                "read plugin package entry: {error}"
            )))),
            // `.staging-*` 是 stage 过程中的半成品（manifest 已写、资源可能还没
            // 拷完）。把它当成包会读到一个内容不全的插件——扫描和 stage 并发
            // 时这是必然会撞上的窗口，不是偶发。
            Ok(entry)
                if entry
                    .file_name()
                    .to_str()
                    .is_none_or(|name| name.starts_with('.')) =>
            {
                None
            }
            Ok(entry) => match entry.file_type() {
                Ok(file_type) if file_type.is_dir() => Some(PluginPackage::load(entry.path())),
                Ok(file_type) if file_type.is_symlink() => Some(Err(HostError::new(format!(
                    "plugin root contains a symlink: {}",
                    entry.path().display()
                )))),
                Ok(_) => None,
                Err(error) => Some(Err(HostError::new(format!(
                    "read plugin package type {}: {error}",
                    entry.path().display()
                )))),
            },
        })
        .collect()
}

/// 把一份 first-party 插件组装成 host 可加载的 package 目录。
///
/// `dest_root/<plugin-id>/{plugin.json,bin/...}`，入口必须是普通文件，不能是
/// symlink；所有入口都会作为包数据以不可执行权限复制。
pub fn stage_plugin_package(
    dest_root: &Path,
    manifest_json: &str,
    entrypoint_src: &Path,
) -> Result<PathBuf, HostError> {
    stage_plugin_package_with_assets(dest_root, manifest_json, entrypoint_src, &[])
}

/// 与 `stage_plugin_package` 相同，另外把若干资源目录原样拷进包里。
///
/// `assets` 的每一项是（包内相对路径，源文件或目录）。贡献 UI 的插件用它把
/// sidecar 和网页装进包内——这样"升级插件"就是替换整个包目录，而不需要重编译宿主。
pub fn stage_plugin_package_with_assets(
    dest_root: &Path,
    manifest_json: &str,
    entrypoint_src: &Path,
    assets: &[(&str, &Path)],
) -> Result<PathBuf, HostError> {
    let manifest: PluginManifest = serde_json::from_str(manifest_json)
        .map_err(|error| HostError::new(format!("decode plugin manifest: {error}")))?;
    manifest
        .validate()
        .map_err(|error| HostError::new(format!("validate plugin manifest: {error}")))?;
    let metadata = fs::symlink_metadata(entrypoint_src).map_err(|error| {
        HostError::new(format!(
            "read plugin entrypoint {}: {error}",
            entrypoint_src.display()
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(HostError::new("plugin entrypoint must be a regular file"));
    }
    fs::create_dir_all(dest_root)?;
    let package_root = dest_root.join(manifest.id.as_str());
    let staging = dest_root.join(format!(".staging-{}", uuid::Uuid::new_v4().simple()));
    let staged = (|| {
        fs::create_dir(&staging)?;
        fs::write(staging.join("plugin.json"), manifest_json.as_bytes())?;
        let entrypoint = staging.join(&manifest.entrypoint);
        if let Some(parent) = entrypoint.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(entrypoint_src, &entrypoint)?;
        fs::set_permissions(&entrypoint, fs::Permissions::from_mode(0o644))?;
        copy_entrypoint_modules(entrypoint_src, &entrypoint)?;
        for (relative, source) in assets {
            copy_asset_tree(source, &staging.join(relative))?;
        }
        PluginPackage::load(&staging)?;
        // 先把旧包挪开再换上新包，而不是删掉再改名：后者会留下一段
        // "包目录不存在"的窗口，并发扫描正好落进去就会看到插件消失。
        let retired = package_root
            .exists()
            .then(|| dest_root.join(format!(".retired-{}", uuid::Uuid::new_v4().simple())));
        if let Some(retired) = &retired {
            fs::rename(&package_root, retired)?;
        }
        match fs::rename(&staging, &package_root) {
            Ok(()) => {
                if let Some(retired) = &retired {
                    let _ = fs::remove_dir_all(retired);
                }
            }
            Err(error) => {
                // 换新失败就把旧包放回去，不能让插件凭空消失。
                if let Some(retired) = &retired {
                    let _ = fs::rename(retired, &package_root);
                }
                return Err(error.into());
            }
        }
        Ok::<(), HostError>(())
    })();
    if let Err(error) = staged {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    Ok(dest_root.to_path_buf())
}

fn is_packaged_module_file(name: &str) -> bool {
    if name.starts_with('.') {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    !lower.ends_with(".test.ts")
        && !lower.ends_with(".test.js")
        && !lower.ends_with(".spec.ts")
        && !lower.ends_with(".spec.js")
}

/// Copy sibling modules next to a Shared Bun entrypoint.
///
/// The host only used to copy the declared entrypoint file. A TypeScript plugin
/// that `import`s `./api.ts` then failed at runtime even though the source tree
/// was complete. Only files in the packaged `bin/` directory are copied, so
/// fixture files sitting beside a loose test binary are left alone.
fn copy_entrypoint_modules(entrypoint_src: &Path, entrypoint_dest: &Path) -> Result<(), HostError> {
    let Some(src_dir) = entrypoint_src.parent() else {
        return Ok(());
    };
    let Some(dest_dir) = entrypoint_dest.parent() else {
        return Ok(());
    };
    if src_dir.file_name() != dest_dir.file_name() {
        return Ok(());
    }
    for entry in fs::read_dir(src_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path == entrypoint_src {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !is_packaged_module_file(name) {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        let dest = dest_dir.join(name);
        fs::copy(&path, &dest)?;
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o644))?;
    }
    Ok(())
}

pub fn executable_fingerprint(path: &Path) -> Result<String, HostError> {
    let mut file = File::open(path).map_err(|error| {
        HostError::new(format!(
            "open executable fingerprint source {}: {error}",
            path.display()
        ))
    })?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            HostError::new(format!(
                "hash executable fingerprint source {}: {error}",
                path.display()
            ))
        })?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

struct BundledPackageDir {
    id: PluginId,
    root: PathBuf,
}

/// 安装器拷贝 bundled 插件时只读 `id`，不跑当前版 `PluginPackage::load`。
/// 完整清单由新守护自己解析；否则下一版多一个字段就会让旧安装器整包失败。
fn peek_plugin_package_id(root: &Path) -> Result<PluginId, HostError> {
    let manifest_path = root.join("plugin.json");
    let metadata = fs::symlink_metadata(&manifest_path)
        .map_err(|error| HostError::new(format!("read plugin manifest metadata: {error}")))?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_MANIFEST_BYTES
    {
        return Err(HostError::new(
            "plugin manifest is not a bounded regular file",
        ));
    }
    let bytes = fs::read(&manifest_path)
        .map_err(|error| HostError::new(format!("read plugin manifest: {error}")))?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| HostError::new(format!("decode plugin manifest: {error}")))?;
    let Some(id) = value.get("id").and_then(serde_json::Value::as_str) else {
        return Err(HostError::new("plugin manifest is missing id"));
    };
    PluginId::new(id).map_err(|error| HostError::new(format!("plugin id is invalid: {error}")))
}

fn discover_bundled_package_dirs(root: &Path) -> Result<Vec<BundledPackageDir>, HostError> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut entries = fs::read_dir(root)
        .map_err(|error| HostError::new(format!("read plugin root {}: {error}", root.display())))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| HostError::new(format!("read plugin package entry: {error}")))?;
    entries.sort_by_key(|entry| entry.file_name());
    let mut packages = Vec::new();
    let mut seen = BTreeSet::new();
    for entry in entries {
        let name = entry.file_name();
        if name.to_str().is_none_or(|name| name.starts_with('.')) {
            continue;
        }
        let path = entry.path();
        let file_type = entry.file_type().map_err(|error| {
            HostError::new(format!(
                "read plugin package type {}: {error}",
                path.display()
            ))
        })?;
        if file_type.is_symlink() {
            return Err(HostError::new(format!(
                "plugin root contains a symlink: {}",
                path.display()
            )));
        }
        if !file_type.is_dir() {
            continue;
        }
        let id = peek_plugin_package_id(&path)?;
        if !seen.insert(id.clone()) {
            return Err(HostError::new(format!(
                "duplicate bundled plugin id: {}",
                id.as_str()
            )));
        }
        packages.push(BundledPackageDir { id, root: path });
    }
    packages.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(packages)
}

/// Copies one immutable bundled plugin set and atomically maps it to a daemon build.
///
/// 拷贝按文件树进行，不把当前宿主的插件 schema 当成更新闸门。新守护启动后再
/// `PluginPackage::load`。
pub fn sync_bundled_plugin_set(
    source_root: Option<&Path>,
    smelt_root: &Path,
    daemon_executable: &Path,
) -> Result<PathBuf, HostError> {
    let packages = match source_root {
        Some(root) if root.is_dir() => discover_bundled_package_dirs(root)?,
        Some(root) if root.exists() => {
            return Err(HostError::new(format!(
                "bundled plugin root is not a directory: {}",
                root.display()
            )));
        }
        _ => Vec::new(),
    };
    let plugin_set_id = fingerprint_bundled_packages(&packages)?;
    let daemon_id = executable_fingerprint(daemon_executable)?;
    let runtime_root = smelt_root.join(PLUGIN_RUNTIME_DIR);
    let sets_root = runtime_root.join(PLUGIN_SETS_DIR);
    let destination = sets_root.join(&plugin_set_id);
    fs::create_dir_all(&sets_root)?;

    if !plugin_set_matches(&destination, &plugin_set_id) {
        if destination.exists() {
            let incomplete = sets_root.join(format!(
                ".incomplete-{}-{}",
                plugin_set_id,
                uuid::Uuid::new_v4().simple()
            ));
            fs::rename(&destination, &incomplete).map_err(|error| {
                HostError::new(format!(
                    "quarantine incomplete plugin set {}: {error}",
                    destination.display()
                ))
            })?;
        }
        let staging = sets_root.join(format!(".staging-{}", uuid::Uuid::new_v4().simple()));
        let staged = (|| {
            fs::create_dir(&staging)?;
            for package in &packages {
                let target = staging.join(package.id.as_str());
                copy_package(&package.root, &target)?;
            }
            fs::write(staging.join(COMPLETE_MARKER), format!("{plugin_set_id}\n"))?;
            fs::rename(&staging, &destination)?;
            Ok::<(), HostError>(())
        })();
        if let Err(error) = staged {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
    }

    let mappings = runtime_root.join(DAEMON_SETS_DIR);
    fs::create_dir_all(&mappings)?;
    atomic_write(
        &mappings.join(daemon_id),
        format!("{plugin_set_id}\n").as_bytes(),
    )?;
    Ok(destination)
}

/// Resolves the immutable plugin set selected for this exact daemon executable.
pub fn active_plugin_set_root(
    smelt_root: &Path,
    daemon_executable: &Path,
) -> Result<Option<PathBuf>, HostError> {
    let daemon_id = executable_fingerprint(daemon_executable)?;
    active_plugin_set_root_for_daemon_id(smelt_root, &daemon_id)
}

/// Resolves a plugin set from a daemon fingerprint captured before an exec/rename handoff.
pub fn active_plugin_set_root_for_daemon_id(
    smelt_root: &Path,
    daemon_id: &str,
) -> Result<Option<PathBuf>, HostError> {
    if !is_sha256_hex(daemon_id) {
        return Err(HostError::new("daemon executable fingerprint is invalid"));
    }
    let runtime_root = smelt_root.join(PLUGIN_RUNTIME_DIR);
    let mapping = runtime_root.join(DAEMON_SETS_DIR).join(daemon_id);
    let plugin_set_id = match fs::read_to_string(&mapping) {
        Ok(value) => value.trim().to_string(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(HostError::new(format!(
                "read active plugin set {}: {error}",
                mapping.display()
            )));
        }
    };
    if !is_sha256_hex(&plugin_set_id) {
        return Err(HostError::new("active plugin set id is invalid"));
    }
    let root = runtime_root.join(PLUGIN_SETS_DIR).join(&plugin_set_id);
    if !plugin_set_matches(&root, &plugin_set_id) {
        return Err(HostError::new(format!(
            "active plugin set is incomplete: {}",
            root.display()
        )));
    }
    Ok(Some(root))
}

/// 规范化的整包摘要：路径、可执行位、长度与内容全部参与。
///
/// 用户插件安装时记录它，之后每次加载重算。不要只 hash entrypoint——网页、SDK、
/// manifest 任何一处被改都必须让摘要变化。
pub fn package_fingerprint(package: &PluginPackage) -> Result<String, HostError> {
    plugin_set_fingerprint(std::slice::from_ref(package))
}

fn plugin_set_fingerprint(packages: &[PluginPackage]) -> Result<String, HostError> {
    fingerprint_package_trees(
        packages
            .iter()
            .map(|package| (package.manifest().id.as_str(), package.root())),
    )
}

fn fingerprint_bundled_packages(packages: &[BundledPackageDir]) -> Result<String, HostError> {
    fingerprint_package_trees(
        packages
            .iter()
            .map(|package| (package.id.as_str(), package.root.as_path())),
    )
}

fn fingerprint_package_trees<'a>(
    packages: impl IntoIterator<Item = (&'a str, &'a Path)>,
) -> Result<String, HostError> {
    let mut digest = Sha256::new();
    digest.update(b"smelt-plugin-set-v1\0");
    for (id, root) in packages {
        hash_one_package(&mut digest, id, root)?;
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn hash_one_package(digest: &mut Sha256, id: &str, root: &Path) -> Result<(), HostError> {
    update_hash_field(digest, id.as_bytes());
    for file in package_files(root)? {
        let relative = file
            .strip_prefix(root)
            .map_err(|error| HostError::new(format!("resolve plugin package path: {error}")))?;
        let relative = relative
            .to_str()
            .ok_or_else(|| HostError::new("plugin package paths must be valid UTF-8"))?;
        update_hash_field(digest, relative.as_bytes());
        let metadata = fs::metadata(&file)?;
        digest.update((metadata.permissions().mode() & 0o111).to_le_bytes());
        digest.update(metadata.len().to_le_bytes());
        let mut input = File::open(&file)?;
        let mut copied = 0_u64;
        let mut buffer = [0_u8; 128 * 1024];
        loop {
            let read = input.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            copied = copied.saturating_add(read as u64);
            digest.update(&buffer[..read]);
        }
        if copied != metadata.len() {
            return Err(HostError::new(format!(
                "plugin package changed while hashing: {}",
                file.display()
            )));
        }
    }
    Ok(())
}

fn update_hash_field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}

fn package_files(root: &Path) -> Result<Vec<PathBuf>, HostError> {
    fn visit(directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), HostError> {
        let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                return Err(HostError::new(format!(
                    "plugin package contains a symlink: {}",
                    path.display()
                )));
            }
            if file_type.is_dir() {
                visit(&path, files)?;
            } else if file_type.is_file() {
                files.push(path);
            } else {
                return Err(HostError::new(format!(
                    "plugin package contains a non-regular file: {}",
                    path.display()
                )));
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    visit(root, &mut files)?;
    Ok(files)
}

fn copy_package(source: &Path, destination: &Path) -> Result<(), HostError> {
    fs::create_dir(destination)?;
    for source_file in package_files(source)? {
        let relative = source_file
            .strip_prefix(source)
            .map_err(|error| HostError::new(error.to_string()))?;
        let destination_file = destination.join(relative);
        if let Some(parent) = destination_file.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&source_file, &destination_file)?;
        let mode = fs::metadata(&source_file)?.permissions().mode() & 0o777;
        fs::set_permissions(&destination_file, fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

fn complete_marker_matches(root: &Path, expected: &str) -> bool {
    fs::read_to_string(root.join(COMPLETE_MARKER))
        .ok()
        .is_some_and(|value| value.trim() == expected)
}

fn plugin_set_matches(root: &Path, expected: &str) -> bool {
    if !complete_marker_matches(root, expected) {
        return false;
    }
    discover_bundled_package_dirs(root)
        .and_then(|packages| fingerprint_bundled_packages(&packages))
        .is_ok_and(|actual| actual == expected)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), HostError> {
    let parent = path
        .parent()
        .ok_or_else(|| HostError::new("atomic write target has no parent"))?;
    let temporary = parent.join(format!(".mapping-{}", uuid::Uuid::new_v4().simple()));
    let result = (|| {
        let mut file = File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
        fs::rename(&temporary, path)?;
        Ok::<(), HostError>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

pub trait PluginProcessVerifier: Send + Sync + 'static {
    /// The executable is always Smelt's managed Bun binary. Package integrity is validated during
    /// discovery before the module path is passed to that process.
    fn verify(&self, package: &PluginPackage, program: &Path, pid: u32) -> Result<(), HostError>;
}

#[derive(Clone, Debug)]
pub struct SpawnOptions {
    pub startup_timeout: std::time::Duration,
    pub shutdown_grace: std::time::Duration,
    pub bun: Option<PathBuf>,
    /// Owned descriptors that the shared Bun process must retain across exec.
    /// Parent copies stay CLOEXEC; only the post-fork child clears that flag.
    pub inherited_fds: Vec<Arc<OwnedFd>>,
}

impl Default for SpawnOptions {
    fn default() -> Self {
        Self {
            startup_timeout: std::time::Duration::from_secs(5),
            shutdown_grace: std::time::Duration::from_secs(2),
            bun: None,
            inherited_fds: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PluginLifecycleState {
    Discovered,
    Starting,
    Ready { pid: u32 },
    Backoff { attempt: u32 },
    Failed,
    Stopped,
    Disabled,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PluginStatus {
    pub plugin_id: PluginId,
    pub name: String,
    pub version: String,
    pub state: PluginLifecycleState,
    pub last_error: Option<String>,
}

fn contribution_accepts_operation(
    contribution: &Contribution,
    operation: &smelt_plugin_api::InvocationOperation,
) -> bool {
    matches!(
        contribution,
        Contribution::Command { operation: declared, .. }
            | Contribution::InputRoute { operation: declared, .. }
            | Contribution::SessionAction { operation: declared, .. }
            if declared == operation
    ) || matches!(
        contribution,
        Contribution::ToolPanel { .. } | Contribution::WorkspaceSurface { .. }
            if operation.as_str() == smelt_plugin_api::CORE_INVOCATION_PANEL_MESSAGE
    )
}

fn write_json_line<T: serde::Serialize>(
    writer: &mut impl Write,
    value: &T,
) -> Result<(), HostError> {
    let mut encoded = serde_json::to_vec(value)
        .map_err(|error| HostError::new(format!("encode plugin wire message: {error}")))?;
    if encoded.len().saturating_add(1) > smelt_plugin_api::PLUGIN_WIRE_MAX_LINE_BYTES {
        return Err(HostError::new("plugin wire message exceeds line limit"));
    }
    encoded.push(b'\n');
    writer.write_all(&encoded)?;
    writer.flush()?;
    Ok(())
}

fn read_json_line<T: serde::de::DeserializeOwned>(
    reader: &mut impl std::io::BufRead,
) -> Result<T, HostError> {
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Err(HostError::new("shared bun control channel closed"));
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |position| position + 1);
        if bytes.len().saturating_add(consumed) > smelt_plugin_api::PLUGIN_WIRE_MAX_LINE_BYTES {
            return Err(HostError::new("plugin wire message exceeds line limit"));
        }
        bytes.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if newline.is_some() {
            bytes.pop();
            return serde_json::from_slice(&bytes)
                .map_err(|error| HostError::new(format!("decode plugin wire message: {error}")));
        }
    }
}

fn set_cloexec(fd: i32, enabled: bool) -> Result<(), HostError> {
    set_cloexec_io(fd, enabled).map_err(HostError::from)
}

fn set_cloexec_io(fd: i32, enabled: bool) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let next = if enabled {
        flags | libc::FD_CLOEXEC
    } else {
        flags & !libc::FD_CLOEXEC
    };
    if unsafe { libc::fcntl(fd, libc::F_SETFD, next) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn terminate_child(child: &mut std::process::Child, grace: Duration) {
    let process_group = child.id();
    let mut leader_reaped = child.try_wait().ok().flatten().is_some();
    let Ok(process_group) = i32::try_from(process_group) else {
        if !leader_reaped {
            let _ = child.kill();
            let _ = child.wait();
        }
        return;
    };
    signal_process_group(process_group, libc::SIGTERM);
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if !leader_reaped {
            leader_reaped = child.try_wait().ok().flatten().is_some();
        }
        if !process_group_exists(process_group) {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    signal_process_group(process_group, libc::SIGKILL);
    if !leader_reaped {
        let _ = child.wait();
    }
}

fn signal_process_group(process_group: i32, signal: i32) {
    if process_group > 1 {
        unsafe {
            libc::kill(-process_group, signal);
        }
    }
}

fn process_group_exists(process_group: i32) -> bool {
    if process_group <= 1 {
        return false;
    }
    if unsafe { libc::kill(-process_group, 0) } == 0 {
        return true;
    }
    io::Error::last_os_error().kind() == io::ErrorKind::PermissionDenied
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn session_action_invocation_must_match_its_declared_operation() {
        let action = Contribution::SessionAction {
            id: smelt_plugin_api::ContributionId::new("open-strategy").unwrap(),
            title: "打开策略".into(),
            operation: smelt_plugin_api::InvocationOperation::new("open_strategy").unwrap(),
            controller: smelt_plugin_api::ContributionId::new("quant-session").unwrap(),
            locations: vec![smelt_plugin_api::SessionActionLocation::SessionMenu],
            icon: None,
            result: smelt_plugin_api::SessionActionResult::OpenExternal,
        };

        assert!(contribution_accepts_operation(
            &action,
            &smelt_plugin_api::InvocationOperation::new("open_strategy").unwrap()
        ));
        assert!(!contribution_accepts_operation(
            &action,
            &smelt_plugin_api::InvocationOperation::new("delete_strategy").unwrap()
        ));
    }

    #[test]
    fn workspace_surface_accepts_the_same_panel_message_as_a_tool_panel() {
        let panel_message = smelt_plugin_api::InvocationOperation::new(
            smelt_plugin_api::CORE_INVOCATION_PANEL_MESSAGE,
        )
        .unwrap();
        let surface = Contribution::WorkspaceSurface {
            id: smelt_plugin_api::ContributionId::new("board").unwrap(),
            title: "Board".into(),
            entry: "web/index.html".into(),
        };
        let panel = Contribution::ToolPanel {
            id: smelt_plugin_api::ContributionId::new("browser").unwrap(),
            title: "浏览器".into(),
            entry: "web/index.html".into(),
        };
        assert!(contribution_accepts_operation(&surface, &panel_message));
        assert!(contribution_accepts_operation(&panel, &panel_message));
        assert!(!contribution_accepts_operation(
            &surface,
            &smelt_plugin_api::InvocationOperation::new("login").unwrap()
        ));
    }

    fn fixture_root(label: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root")
            .join("target/plugin-host-test-fixtures")
            .join(format!(
                "smelt-plugin-host-{label}-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ))
    }

    fn write_manifest(root: &Path, entrypoint: &str) {
        fs::write(
            root.join("plugin.json"),
            serde_json::json!({
                "id": "com.example",
                "name": "Example",
                "version": "1.0.0",
                "api_version": 1,
                "entrypoint": entrypoint,
                "capabilities": []
            })
            .to_string(),
        )
        .unwrap();
    }

    #[test]
    fn stage_plugin_package_copies_a_regular_entrypoint_and_rejects_symlinks() {
        let root = fixture_root("stage-package");
        fs::create_dir_all(&root).unwrap();
        let source = root.join("smelt-plugin-example");
        fs::write(&source, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o755)).unwrap();
        let dest = root.join("packages");
        let manifest = serde_json::json!({
            "id": "com.example",
            "name": "Example",
            "version": "1.0.0",
            "api_version": 1,
            "entrypoint": "bin/example",
            "capabilities": []
        })
        .to_string();

        let staged = stage_plugin_package(&dest, &manifest, &source).unwrap();
        let package = PluginPackage::load(staged.join("com.example")).unwrap();
        assert_eq!(package.manifest().id.as_str(), "com.example");
        assert!(
            !package
                .entrypoint()
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read(package.entrypoint()).unwrap(),
            b"#!/bin/sh\nexit 0\n"
        );

        symlink("/bin/sh", root.join("linked")).unwrap();
        let error = stage_plugin_package(&dest, &manifest, &root.join("linked")).unwrap_err();
        assert!(error.to_string().contains("regular file"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stage_plugin_package_copies_bun_modules_beside_the_entrypoint() {
        let root = fixture_root("stage-modules");
        let bin = root.join("src/bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("main.ts"), b"export { x } from './api.ts';\n").unwrap();
        fs::write(bin.join("api.ts"), b"export const x = 1;\n").unwrap();
        fs::write(bin.join("api.test.ts"), b"throw new Error('test');\n").unwrap();
        let dest = root.join("packages");
        let manifest = serde_json::json!({
            "id": "com.example",
            "name": "Example",
            "version": "1.0.0",
            "api_version": 1,
            "entrypoint": "bin/main.ts",
            "capabilities": []
        })
        .to_string();

        stage_plugin_package(&dest, &manifest, &bin.join("main.ts")).unwrap();
        let package = dest.join("com.example/bin");
        assert_eq!(
            fs::read(package.join("api.ts")).unwrap(),
            b"export const x = 1;\n"
        );
        assert!(
            !package.join("api.test.ts").exists(),
            "unit tests must not be staged into the runtime package"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn staging_leftovers_are_not_discovered_as_packages() {
        let root = fixture_root("staging-skip");
        let dest = root.join("packages");
        fs::create_dir_all(&dest).unwrap();

        // 一个半成品 staging 目录：manifest 已经写了，bin 还没拷。
        let staging = dest.join(".staging-deadbeef");
        fs::create_dir_all(&staging).unwrap();
        write_manifest(&staging, "bin/example");

        assert!(
            discover_packages(&dest).is_empty(),
            "点开头的中间目录不能被当成插件包"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restaging_never_leaves_the_package_missing_or_partial() {
        let root = fixture_root("restage");
        fs::create_dir_all(&root).unwrap();
        let source = root.join("example-bin");
        fs::write(&source, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o755)).unwrap();
        let assets = root.join("web");
        fs::create_dir_all(&assets).unwrap();
        fs::write(assets.join("index.html"), b"v1").unwrap();
        let dest = root.join("packages");
        let manifest = serde_json::json!({
            "id": "com.example",
            "name": "Example",
            "version": "1.0.0",
            "api_version": 1,
            "entrypoint": "bin/example",
            "capabilities": []
        })
        .to_string();

        let assets_arg: Vec<(&str, &Path)> = vec![("web", assets.as_path())];
        stage_plugin_package_with_assets(&dest, &manifest, &source, &assets_arg).unwrap();
        let package = PluginPackage::load(dest.join("com.example")).unwrap();
        assert_eq!(
            fs::read(package.resolve_asset("web/index.html").unwrap()).unwrap(),
            b"v1"
        );

        // 升级：同一个包换新内容，旧包被换掉而不是先删后建。
        fs::write(assets.join("index.html"), b"v2").unwrap();
        stage_plugin_package_with_assets(&dest, &manifest, &source, &assets_arg).unwrap();
        let package = PluginPackage::load(dest.join("com.example")).unwrap();
        assert_eq!(
            fs::read(package.resolve_asset("web/index.html").unwrap()).unwrap(),
            b"v2"
        );
        // 换新之后不留任何中间目录。
        let leftovers = fs::read_dir(&dest)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with('.'))
            .count();
        assert_eq!(leftovers, 0, "换新后不该留下 staging/retired 目录");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn package_assets_cannot_escape_the_package_root() {
        let root = fixture_root("asset-escape");
        fs::create_dir_all(root.join("bin")).unwrap();
        let executable = root.join("bin/example");
        fs::write(&executable, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        write_manifest(&root, "bin/example");
        fs::create_dir_all(root.join("web")).unwrap();
        fs::write(root.join("web/index.html"), b"ok").unwrap();
        // 包内指向包外的符号链接：canonicalize 之后必须落在包外而被拒。
        symlink("/etc/passwd", root.join("web/leak")).unwrap();

        let package = PluginPackage::load(&root).unwrap();
        assert!(package.resolve_asset("web/index.html").is_some());
        assert!(package.resolve_asset("../../etc/passwd").is_none());
        assert!(package.resolve_asset("web/leak").is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn package_load_resolves_a_bounded_entrypoint_inside_the_package() {
        let root = fixture_root("valid");
        fs::create_dir_all(root.join("bin")).unwrap();
        let executable = root.join("bin/example");
        fs::write(&executable, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        write_manifest(&root, "bin/example");

        let package = PluginPackage::load(&root).unwrap();
        assert_eq!(package.manifest().id.as_str(), "com.example");
        assert!(package.entrypoint().starts_with(package.root()));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn package_load_merges_known_ui_and_ignores_future_contribution_types() {
        let root = fixture_root("ui-sidecar");
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(root.join("web")).unwrap();
        let executable = root.join("bin/example");
        fs::write(&executable, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(root.join("web/index.html"), b"ok").unwrap();
        fs::write(
            root.join("plugin.json"),
            serde_json::json!({
                "id": "com.example",
                "name": "Example",
                "version": "1.0.0",
                "api_version": 1,
                "entrypoint": "bin/example",
                "capabilities": [smelt_plugin_api::CORE_CAPABILITY_UI_CONTRIBUTE],
                "contributions": []
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            root.join(PLUGIN_UI_MANIFEST_FILE),
            serde_json::json!({
                "contributions": [
                    { "type": "future_surface", "id": "ignored" },
                    {
                        "type": "tool_panel",
                        "id": "browser",
                        "title": "Browser",
                        "entry": "web/index.html"
                    },
                    {
                        "type": "workspace_surface",
                        "id": "board",
                        "title": "Board",
                        "entry": "web/index.html"
                    }
                ]
            })
            .to_string(),
        )
        .unwrap();

        let package = PluginPackage::load(&root).expect("未知 UI 类型不应阻断插件主体");
        assert_eq!(package.manifest().contributions.len(), 2);
        assert!(matches!(
            &package.manifest().contributions[0],
            Contribution::ToolPanel { entry, .. } if entry == "web/index.html"
        ));
        assert!(matches!(
            &package.manifest().contributions[1],
            Contribution::WorkspaceSurface { id, .. } if id.as_str() == "board"
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn package_load_merges_settings_before_the_dependent_sidebar_account() {
        let root = fixture_root("settings-ui-sidecar");
        fs::create_dir_all(root.join("bin")).unwrap();
        let executable = root.join("bin/example");
        fs::write(&executable, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            root.join("plugin.json"),
            serde_json::json!({
                "id": "com.example",
                "name": "Example",
                "version": "1.0.0",
                "api_version": 1,
                "entrypoint": "bin/example",
                "capabilities": [smelt_plugin_api::CORE_CAPABILITY_UI_CONTRIBUTE],
                "contributions": [{
                    "type": "command",
                    "id": "settings-snapshot",
                    "title": "Read settings",
                    "operation": "get_settings_view"
                }]
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            root.join(PLUGIN_UI_MANIFEST_FILE),
            serde_json::json!({
                "contributions": [
                    {
                        "type": "sidebar_account",
                        "id": "account-slot",
                        "settings_section": "settings",
                        "account_item": "account"
                    },
                    {
                        "type": "settings_section",
                        "id": "settings",
                        "title": "Example",
                        "snapshot": "settings-snapshot"
                    }
                ]
            })
            .to_string(),
        )
        .unwrap();

        let package = PluginPackage::load(&root).unwrap();
        assert!(package.manifest().contributions.iter().any(|contribution| {
            matches!(contribution, Contribution::SettingsSection { id, .. } if id.as_str() == "settings")
        }));
        assert!(package.manifest().contributions.iter().any(|contribution| {
            matches!(contribution, Contribution::SidebarAccount { id, .. } if id.as_str() == "account-slot")
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn package_load_rejects_new_ui_types_in_the_legacy_base_manifest() {
        let root = fixture_root("ui-in-base-manifest");
        fs::create_dir_all(root.join("bin")).unwrap();
        let executable = root.join("bin/example");
        fs::write(&executable, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            root.join("plugin.json"),
            serde_json::json!({
                "id": "com.example",
                "name": "Example",
                "version": "1.0.0",
                "api_version": 1,
                "entrypoint": "bin/example",
                "capabilities": [smelt_plugin_api::CORE_CAPABILITY_UI_CONTRIBUTE],
                "contributions": [{
                    "type": "tool_panel",
                    "id": "browser",
                    "title": "Browser",
                    "entry": "web/index.html"
                }]
            })
            .to_string(),
        )
        .unwrap();

        let error = PluginPackage::load(&root).unwrap_err();
        assert!(error.to_string().contains(PLUGIN_UI_MANIFEST_FILE));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn package_load_rejects_input_routes_in_the_legacy_base_manifest() {
        let root = fixture_root("input-route-in-base-manifest");
        fs::create_dir_all(root.join("bin")).unwrap();
        let executable = root.join("bin/example");
        fs::write(&executable, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            root.join("plugin.json"),
            serde_json::json!({
                "id": "com.example",
                "name": "Example",
                "version": "1.0.0",
                "api_version": 1,
                "entrypoint": "bin/example",
                "capabilities": [smelt_plugin_api::CORE_CAPABILITY_SESSION_INPUT_ROUTE],
                "contributions": [{
                    "type": "input_route",
                    "id": "conversation-input",
                    "operation": "submit_input"
                }]
            })
            .to_string(),
        )
        .unwrap();

        let error = PluginPackage::load(&root).unwrap_err();
        assert!(error.to_string().contains(PLUGIN_INPUT_MANIFEST_FILE));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn package_load_merges_known_input_routes_and_ignores_future_types() {
        let root = fixture_root("input-sidecar");
        fs::create_dir_all(root.join("bin")).unwrap();
        let executable = root.join("bin/example");
        fs::write(&executable, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            root.join("plugin.json"),
            serde_json::json!({
                "id": "com.example",
                "name": "Example",
                "version": "1.0.0",
                "api_version": 1,
                "entrypoint": "bin/example",
                "capabilities": [smelt_plugin_api::CORE_CAPABILITY_SESSION_INPUT_ROUTE],
                "contributions": []
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            root.join(PLUGIN_INPUT_MANIFEST_FILE),
            serde_json::json!({
                "contributions": [
                    { "type": "future_route", "id": "ignored" },
                    {
                        "type": "input_route",
                        "id": "conversation-input",
                        "operation": "submit_input"
                    }
                ]
            })
            .to_string(),
        )
        .unwrap();

        let package = PluginPackage::load(&root).expect("未知输入路由类型不应阻断插件主体");
        assert_eq!(package.manifest().contributions.len(), 1);
        assert!(matches!(
            &package.manifest().contributions[0],
            Contribution::InputRoute { id, operation }
                if id.as_str() == "conversation-input" && operation.as_str() == "submit_input"
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn package_load_merges_agent_and_session_controller_sidecar() {
        let root = fixture_root("agent-sidecar");
        fs::create_dir_all(root.join("bin")).unwrap();
        let executable = root.join("bin/example");
        fs::write(&executable, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            root.join("plugin.json"),
            serde_json::json!({
                "id": "com.example",
                "name": "Example",
                "version": "1.0.0",
                "api_version": 1,
                "entrypoint": "bin/example",
                "capabilities": [
                    smelt_plugin_api::CORE_CAPABILITY_SESSION_INPUT_ROUTE,
                    smelt_plugin_api::CORE_CAPABILITY_AGENT_CONTRIBUTE
                ],
                "contributions": []
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            root.join(PLUGIN_INPUT_MANIFEST_FILE),
            serde_json::json!({
                "contributions": [{
                    "type": "input_route",
                    "id": "conversation-input",
                    "operation": "submit_input"
                }]
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            root.join(PLUGIN_AGENT_MANIFEST_FILE),
            serde_json::json!({
                "contributions": [
                    {
                        "type": "agent",
                        "id": "researcher",
                        "name": "Researcher",
                        "controller": "remote-session"
                    },
                    {
                        "type": "session_controller",
                        "id": "remote-session",
                        "input_route": "conversation-input"
                    }
                ]
            })
            .to_string(),
        )
        .unwrap();

        let package = PluginPackage::load(&root).expect("agent sidecar should be discovered");
        assert_eq!(package.manifest().contributions.len(), 3);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn package_load_rejects_an_entrypoint_symlink_that_escapes() {
        let root = fixture_root("escape");
        fs::create_dir_all(root.join("bin")).unwrap();
        symlink("/bin/sh", root.join("bin/example")).unwrap();
        write_manifest(&root, "bin/example");

        let error = PluginPackage::load(&root).unwrap_err();
        assert!(error.to_string().contains("symlink"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bundled_plugin_sets_are_content_addressed_and_selected_per_daemon() {
        let root = fixture_root("managed-set");
        let source = root.join("bundled");
        let package = source.join("com.example");
        fs::create_dir_all(package.join("bin")).unwrap();
        let executable = package.join("bin/example");
        fs::write(&executable, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(package.join("asset.txt"), b"first").unwrap();
        fs::write(
            package.join(PLUGIN_UI_MANIFEST_FILE),
            serde_json::json!({ "contributions": [] }).to_string(),
        )
        .unwrap();
        write_manifest(&package, "bin/example");
        let daemon = root.join("smeltd");
        fs::write(&daemon, b"daemon-build-a").unwrap();
        let smelt_root = root.join("state");

        let first = sync_bundled_plugin_set(Some(&source), &smelt_root, &daemon).unwrap();
        assert_eq!(
            active_plugin_set_root(&smelt_root, &daemon).unwrap(),
            Some(first.clone())
        );
        assert!(PluginPackage::load(first.join("com.example")).is_ok());
        assert!(
            first
                .join("com.example")
                .join(PLUGIN_UI_MANIFEST_FILE)
                .is_file(),
            "不可变插件集必须保留 UI sidecar"
        );

        fs::write(first.join("com.example/asset.txt"), b"tampered").unwrap();
        assert!(active_plugin_set_root(&smelt_root, &daemon).is_err());
        let restored = sync_bundled_plugin_set(Some(&source), &smelt_root, &daemon).unwrap();
        assert_eq!(restored, first);
        assert_eq!(
            fs::read(restored.join("com.example/asset.txt")).unwrap(),
            b"first"
        );

        fs::write(package.join("asset.txt"), b"second").unwrap();
        let second = sync_bundled_plugin_set(Some(&source), &smelt_root, &daemon).unwrap();
        assert_ne!(first, second);
        assert!(first.is_dir(), "old immutable set must remain intact");
        assert_eq!(
            active_plugin_set_root(&smelt_root, &daemon).unwrap(),
            Some(second)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bundled_plugin_sync_does_not_use_current_manifest_schema_as_an_update_gate() {
        let root = fixture_root("future-manifest-set");
        let source = root.join("bundled");
        let package = source.join("com.example");
        fs::create_dir_all(package.join("bin")).unwrap();
        let entrypoint = package.join("bin/main.ts");
        fs::write(&entrypoint, b"export {}").unwrap();
        fs::set_permissions(&entrypoint, fs::Permissions::from_mode(0o644)).unwrap();
        fs::write(
            package.join("plugin.json"),
            serde_json::json!({
                "id": "com.example",
                "name": "Example",
                "version": "1.0.0",
                "api_version": 1,
                "entrypoint": "bin/main.ts",
                "future_field": true
            })
            .to_string(),
        )
        .unwrap();
        let daemon = root.join("smeltd");
        fs::write(&daemon, b"daemon-build-future").unwrap();
        let smelt_root = root.join("state");

        let selected = sync_bundled_plugin_set(Some(&source), &smelt_root, &daemon).unwrap();
        assert_eq!(
            active_plugin_set_root(&smelt_root, &daemon).unwrap(),
            Some(selected.clone())
        );
        assert_eq!(
            fs::read(selected.join("com.example/bin/main.ts")).unwrap(),
            b"export {}"
        );
        let error = PluginPackage::load(selected.join("com.example")).unwrap_err();
        assert!(
            error.to_string().contains("unknown field"),
            "运行时仍按当前 schema load：{error}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_empty_bundled_set_clears_a_daemon_mapping_without_legacy_fallback() {
        let root = fixture_root("empty-set");
        fs::create_dir_all(&root).unwrap();
        let daemon = root.join("smeltd");
        fs::write(&daemon, b"daemon-build").unwrap();
        let smelt_root = root.join("state");

        let selected = sync_bundled_plugin_set(None, &smelt_root, &daemon).unwrap();
        assert_eq!(
            active_plugin_set_root(&smelt_root, &daemon).unwrap(),
            Some(selected.clone())
        );
        assert!(discover_packages(selected).is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn daemon_fingerprint_resolves_its_plugin_set_after_the_executable_is_renamed() {
        let root = fixture_root("renamed-daemon");
        let daemon = root.join("smeltd.next");
        fs::create_dir_all(&root).unwrap();
        fs::write(&daemon, b"daemon-build").unwrap();
        let daemon_id = executable_fingerprint(&daemon).unwrap();
        let smelt_root = root.join("state");
        let selected = sync_bundled_plugin_set(None, &smelt_root, &daemon).unwrap();
        fs::rename(&daemon, root.join("smeltd")).unwrap();

        assert_eq!(
            active_plugin_set_root_for_daemon_id(&smelt_root, &daemon_id).unwrap(),
            Some(selected)
        );
        fs::remove_dir_all(root).unwrap();
    }
}
