//! Provisioning for Smelt's immutable Bun and Pi runtime generations.
//!
//! 发布包在打包时装好 Pi 生产依赖，把归档和 Bun zip 放进应用。首次启动只校验 SHA-256
//! 并解压进 generation store。debug 直接用仓库里的 `pi-runtime/` 和 `.cache/bun/bun`。

#![cfg(unix)]

#[cfg(any(test, not(debug_assertions)))]
use crate::runtime_generation::PiGenerationManifest;
use crate::runtime_generation::{
    BunGenerationManifest, GENERATION_SCHEMA, GenerationKind, GenerationLease, GenerationStore,
    RuntimeManagerLock, inherit_generation_fds,
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    process::Command,
};

pub(crate) const BUN_VERSION: &str = "1.4.0";

#[cfg(any(target_os = "macos", target_os = "linux", test))]
/// 当前受管 Bun 的固定入口，相对 runtime 根目录。generation 目录名带哈希、随版本变，
/// 写进外部配置（如 `mcp.json` 的 `command`）的路径必须是这一条。
const BUN_LAUNCHER: &str = "bin/bun";

pub use smelt_model::agent_paths::MANAGED_BUN_LAUNCHER_HOME_PATH;
#[cfg(any(test, not(debug_assertions)))]
pub(crate) const PI_BRIDGE_PROTOCOL: &str = "smelt-pi-rpc-v1";
pub(crate) const PI_RUNTIME_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(not(debug_assertions))]
include!(concat!(env!("OUT_DIR"), "/pi_runtime_archive.rs"));

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const BUN_DOWNLOAD: (&str, &str) = (
    "https://github.com/oven-sh/bun/releases/download/bun-v1.4.0/bun-darwin-aarch64.zip",
    "c669e97f6164e1c96e0701748db98dfa77492908cbd8394c7557134a735de381",
);
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const BUN_DOWNLOAD: (&str, &str) = (
    "https://github.com/oven-sh/bun/releases/download/bun-v1.4.0/bun-darwin-x64.zip",
    "1d0211b8f1dc991182344687ad15e72ee86f154845a5f7fa477994cd341dd9b0",
);
#[cfg(all(
    any(test, not(debug_assertions)),
    target_os = "macos",
    target_arch = "aarch64"
))]
const BUN_ZIP_DIR: &str = "bun-darwin-aarch64";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub(crate) const BUN_EXECUTABLE_SHA256: &str =
    "539598c775882420b9d8deb7dc14d845f20f7d26f5600c50ab067dde6ac3f3bf";
#[cfg(all(
    any(test, not(debug_assertions)),
    target_os = "macos",
    target_arch = "x86_64"
))]
const BUN_ZIP_DIR: &str = "bun-darwin-x64";
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub(crate) const BUN_EXECUTABLE_SHA256: &str =
    "ca8a18d0116d7b6b19f53bb0d8c48e487c0757cab4dc3f4f8cc5e43a44cd75d8";
// Linux x64 用 baseline 版：不要求 AVX2，云主机的老 CPU 也能跑。
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const BUN_DOWNLOAD: (&str, &str) = (
    "https://github.com/oven-sh/bun/releases/download/bun-v1.4.0/bun-linux-x64-baseline.zip",
    "184fb4595f0d401a217cf7c78c1bc430ba83314dab7a8b94805babbf7fa7097f",
);
#[cfg(all(
    any(test, not(debug_assertions)),
    target_os = "linux",
    target_arch = "x86_64"
))]
const BUN_ZIP_DIR: &str = "bun-linux-x64-baseline";
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub(crate) const BUN_EXECUTABLE_SHA256: &str =
    "33d56b070be6a9e3da0ab013038b43d1645d0534ca811ecdba4472599117eb4b";
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const BUN_DOWNLOAD: (&str, &str) = (
    "https://github.com/oven-sh/bun/releases/download/bun-v1.4.0/bun-linux-aarch64.zip",
    "4b1a332ee861983eb93bcfe6f770fff94e3e31b2c388bdaea3c8ed35e58eed0e",
);
#[cfg(all(
    any(test, not(debug_assertions)),
    target_os = "linux",
    target_arch = "aarch64"
))]
const BUN_ZIP_DIR: &str = "bun-linux-aarch64";
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub(crate) const BUN_EXECUTABLE_SHA256: &str =
    "086c4121c8738a8e0f5ed730e8a461bc3973b4444e372ddb77aef9a747fa2ae9";

#[derive(Clone, Debug)]
pub struct ManagedBunRuntime {
    pub path: PathBuf,
    pub generation_id: String,
    lease: GenerationLease,
}

impl ManagedBunRuntime {
    pub fn inherit_into(&self, command: &mut Command) {
        inherit_generation_fds(command, std::slice::from_ref(&self.lease));
    }

    pub fn inherited_fd(&self) -> std::sync::Arc<std::os::fd::OwnedFd> {
        self.lease.inherited_fd()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ManagedPiRuntime {
    pub bun: PathBuf,
    pub root: PathBuf,
    pub entry: PathBuf,
    bun_runtime: ManagedBunRuntime,
    /// debug 直接引用仓库目录，不占 Pi generation lease。
    pi_lease: Option<GenerationLease>,
}

impl ManagedPiRuntime {
    pub(crate) fn process_args(&self, trailing: impl IntoIterator<Item = String>) -> Vec<String> {
        std::iter::once(self.bun.to_string_lossy().into_owned())
            .chain(std::iter::once(self.entry.to_string_lossy().into_owned()))
            .chain(trailing)
            .collect()
    }

    pub(crate) fn inherit_into(&self, command: &mut Command) {
        let mut leases = vec![self.bun_runtime.lease.clone()];
        if let Some(lease) = &self.pi_lease {
            leases.push(lease.clone());
        }
        inherit_generation_fds(command, &leases);
    }
}

fn sha256_parts<'a>(parts: impl IntoIterator<Item = &'a [u8]>) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update((part.len() as u64).to_le_bytes());
        digest.update(part);
    }
    format!("{:x}", digest.finalize())
}

fn bun_manifest() -> BunGenerationManifest {
    let (download_url, archive_sha256) = BUN_DOWNLOAD;
    let identity = sha256_parts([
        BUN_VERSION.as_bytes(),
        std::env::consts::OS.as_bytes(),
        std::env::consts::ARCH.as_bytes(),
        BUN_EXECUTABLE_SHA256.as_bytes(),
        download_url.as_bytes(),
        archive_sha256.as_bytes(),
    ]);
    BunGenerationManifest {
        schema: GENERATION_SCHEMA,
        generation_id: format!("bun-{identity}"),
        version: BUN_VERSION.to_string(),
        platform: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        executable_sha256: BUN_EXECUTABLE_SHA256.to_string(),
        download_url: download_url.to_string(),
        archive_sha256: archive_sha256.to_string(),
    }
}

#[cfg(any(test, not(debug_assertions)))]
fn pi_manifest(bun_id: &str, archive_sha256: &str) -> PiGenerationManifest {
    let identity = sha256_parts([
        PI_RUNTIME_VERSION.as_bytes(),
        archive_sha256.as_bytes(),
        std::env::consts::OS.as_bytes(),
        std::env::consts::ARCH.as_bytes(),
        PI_BRIDGE_PROTOCOL.as_bytes(),
        bun_id.as_bytes(),
    ]);
    PiGenerationManifest {
        schema: GENERATION_SCHEMA,
        generation_id: format!("pi-{identity}"),
        runtime_version: PI_RUNTIME_VERSION.to_string(),
        platform: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        bridge_protocol: PI_BRIDGE_PROTOCOL.to_string(),
        archive_sha256: archive_sha256.to_string(),
        bun_generation_id: bun_id.to_string(),
    }
}

#[cfg(any(test, not(debug_assertions)))]
fn shipped_pi_archive_name() -> String {
    format!(
        "pi-runtime-{}-{}.tar.gz",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

#[cfg(any(test, not(debug_assertions)))]
fn shipped_bun_archive_name() -> String {
    format!(
        "bun-{}-{}.zip",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

fn bun_tree_matches(root: &Path, expected: &BunGenerationManifest) -> bool {
    let bun = root.join("bun");
    let Ok(metadata) = fs::symlink_metadata(&bun) else {
        return false;
    };
    metadata.is_file()
        && !metadata.file_type().is_symlink()
        && metadata.permissions().mode() & 0o111 != 0
        && fs::read(root.join("manifest.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<BunGenerationManifest>(&bytes).ok())
            .as_ref()
            == Some(expected)
}

fn pi_runtime_files_ready(root: &Path) -> bool {
    let rpc_entry =
        root.join("node_modules/@earendil-works/pi-coding-agent/dist/bundle/rpc-entry.js");
    let rpc_mode =
        root.join("node_modules/@earendil-works/pi-coding-agent/dist/modes/rpc/rpc-mode.js");
    root.join("src/main.ts").is_file() && rpc_entry.is_file() && rpc_mode.is_file()
}

#[cfg(any(test, not(debug_assertions)))]
fn pi_tree_matches(root: &Path, expected: &PiGenerationManifest) -> bool {
    pi_runtime_files_ready(root)
        && fs::read(root.join("manifest.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<PiGenerationManifest>(&bytes).ok())
            .as_ref()
            == Some(expected)
}

#[cfg(any(test, not(debug_assertions)))]
fn archive_entry_is_safe(entry: &str) -> bool {
    if entry.is_empty() || entry.contains('\0') {
        return false;
    }
    let path = Path::new(entry);
    if path.is_absolute() {
        return false;
    }
    path.components().all(|component| {
        matches!(
            component,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        )
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn sync_managed_bun(_status: &dyn Fn(&str)) -> Result<ManagedBunRuntime, String> {
    Err("受管 Bun 运行时只在 macOS 和 Linux 上提供".to_string())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn sync_managed_bun(status: &dyn Fn(&str)) -> Result<ManagedBunRuntime, String> {
    let store = GenerationStore::managed()?;
    let manager = store.lock_manager()?;
    let runtime = sync_managed_bun_locked(&store, status, &manager)?;
    drop(manager);
    Ok(runtime)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn sync_managed_bun_locked(
    store: &GenerationStore,
    status: &dyn Fn(&str),
    manager: &RuntimeManagerLock,
) -> Result<ManagedBunRuntime, String> {
    let manifest = bun_manifest();
    store.remove_stale_staging_locked();
    let root = store.generation_root(GenerationKind::Bun, &manifest.generation_id);
    if !bun_tree_matches(&root, &manifest) {
        store.discard_invalid_locked(GenerationKind::Bun, &manifest.generation_id)?;
        store.publish_locked(
            GenerationKind::Bun,
            &manifest.generation_id,
            &manifest,
            |staging| install_managed_bun(staging, status, manager),
        )?;
    }
    let lease = store.acquire_shared_locked(GenerationKind::Bun, &manifest.generation_id)?;
    store.write_current_locked(GenerationKind::Bun, &manifest.generation_id)?;
    // 先把固定入口切到新 generation，再回收旧的：顺序反过来，入口会短暂悬空。
    publish_bun_launcher(store.runtime_root(), &root.join("bun"))?;
    let mut keep = HashSet::new();
    keep.insert(manifest.generation_id.clone());
    store.gc_kind_locked(GenerationKind::Bun, &keep);
    Ok(ManagedBunRuntime {
        path: root.join("bun"),
        generation_id: manifest.generation_id,
        lease,
    })
}

#[cfg(all(debug_assertions, any(target_os = "macos", target_os = "linux")))]
fn install_managed_bun(
    staging: &Path,
    status: &dyn Fn(&str),
    _manager: &RuntimeManagerLock,
) -> Result<(), String> {
    status("正在发布开发用 Bun 运行时…");
    install_bun_from_executable(staging, &workspace_bun_executable()?)
}

#[cfg(all(not(debug_assertions), any(target_os = "macos", target_os = "linux")))]
fn install_managed_bun(
    staging: &Path,
    status: &dyn Fn(&str),
    manager: &RuntimeManagerLock,
) -> Result<(), String> {
    status("正在解压随包的 Bun 运行时…");
    let zip = shipped_runtime_dir()?.join(shipped_bun_archive_name());
    install_bun_from_zip(staging, &zip, BUN_DOWNLOAD.1, manager)
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
/// 把 `<runtime>/bin/bun` 原子地指向 `bun`：先建临时符号链接再 rename 覆盖，
/// 正在通过旧链接启动的进程不会撞上「文件不存在」。
fn publish_bun_launcher(runtime_root: &Path, bun: &Path) -> Result<(), String> {
    let launcher = runtime_root.join(BUN_LAUNCHER);
    if fs::read_link(&launcher).is_ok_and(|target| target == bun) {
        return Ok(());
    }
    let dir = launcher.parent().unwrap_or(runtime_root);
    fs::create_dir_all(dir).map_err(|error| format!("创建 {} 失败：{error}", dir.display()))?;
    let temporary = dir.join(format!(
        ".bun-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let result = std::os::unix::fs::symlink(bun, &temporary)
        .and_then(|()| fs::rename(&temporary, &launcher))
        .map_err(|error| format!("更新 Bun 固定入口 {} 失败：{error}", launcher.display()));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(all(debug_assertions, any(target_os = "macos", target_os = "linux")))]
fn workspace_bun_executable() -> Result<PathBuf, String> {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.cache/bun/bun")
        .canonicalize()
        .map_err(|_| "开发构建需要仓库缓存里的受管 Bun。请先运行 ./scripts/bun.sh".to_string())
}

#[cfg(all(debug_assertions, any(target_os = "macos", target_os = "linux")))]
fn workspace_pi_runtime_root() -> Result<PathBuf, String> {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../pi-runtime")
        .canonicalize()
        .map_err(|error| format!("找不到仓库里的 pi-runtime：{error}"))
}

#[cfg(all(not(debug_assertions), target_os = "macos"))]
fn shipped_runtime_dir() -> Result<PathBuf, String> {
    let exe =
        std::env::current_exe().map_err(|error| format!("读取当前可执行文件失败：{error}"))?;
    let macos = exe.parent().ok_or("可执行文件没有所在目录")?;
    let contents = macos.parent().ok_or("macOS 包结构缺少 Contents")?;
    Ok(contents.join("Resources").join("runtime"))
}

#[cfg(all(not(debug_assertions), target_os = "linux"))]
fn shipped_runtime_dir() -> Result<PathBuf, String> {
    let exe =
        std::env::current_exe().map_err(|error| format!("读取当前可执行文件失败：{error}"))?;
    let dir = exe.parent().ok_or("可执行文件没有所在目录")?;
    Ok(dir.join("runtime"))
}

#[cfg(all(debug_assertions, any(target_os = "macos", target_os = "linux")))]
fn install_bun_from_executable(staging: &Path, source: &Path) -> Result<(), String> {
    let bun = staging.join("bun");
    fs::copy(source, &bun).map_err(|error| format!("复制开发用 Bun 失败：{error}"))?;
    let mut permissions = fs::metadata(&bun)
        .map_err(|error| format!("读取开发用 Bun 权限失败：{error}"))?
        .permissions();
    permissions.set_mode(permissions.mode() | 0o755);
    fs::set_permissions(&bun, permissions)
        .map_err(|error| format!("设置开发用 Bun 可执行权限失败：{error}"))?;
    validate_bun(&bun)
}

#[cfg(all(
    any(target_os = "macos", target_os = "linux"),
    any(test, not(debug_assertions))
))]
fn install_bun_from_zip(
    staging: &Path,
    zip: &Path,
    expected_archive_sha: &str,
    manager: &RuntimeManagerLock,
) -> Result<(), String> {
    if !zip.is_file() {
        return Err(format!("找不到随包的 Bun 压缩包：{}", zip.display()));
    }
    let actual_archive_sha = sha256_file(zip)?;
    if actual_archive_sha != expected_archive_sha {
        return Err(format!(
            "Bun 压缩包校验失败（期望 {expected_archive_sha}，实际 {actual_archive_sha}）"
        ));
    }
    let unpack = staging.join("unpack");
    fs::create_dir(&unpack).map_err(|error| format!("创建 Bun 解压目录失败：{error}"))?;
    let mut unzip = Command::new("unzip");
    unzip.args(["-q"]).arg(zip).arg("-d").arg(&unpack);
    manager.inherit_into(&mut unzip);
    let output = unzip
        .output()
        .map_err(|error| format!("无法执行 unzip：{error}"))?;
    if !output.status.success() {
        return Err(format!(
            "解压 Bun 失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let bun = staging.join("bun");
    fs::rename(unpack.join(BUN_ZIP_DIR).join("bun"), &bun)
        .map_err(|error| format!("安放 Bun 制品失败：{error}"))?;
    fs::remove_dir_all(&unpack).map_err(|error| format!("清理 Bun 解压目录失败：{error}"))?;
    validate_bun(&bun)
}

#[cfg(any(test, not(debug_assertions)))]
fn install_pi_archive(
    staging: &Path,
    archive: &Path,
    expected_sha256: &str,
    manager: &RuntimeManagerLock,
) -> Result<(), String> {
    if !archive.is_file() {
        return Err(format!("找不到随包的 Pi 运行时归档：{}", archive.display()));
    }
    let actual = sha256_file(archive)?;
    if actual != expected_sha256 {
        return Err(format!(
            "Pi 运行时归档校验失败（期望 {expected_sha256}，实际 {actual}）"
        ));
    }
    for entry in list_archive_entries(archive, manager)? {
        if !archive_entry_is_safe(&entry) {
            return Err(format!("Pi 运行时归档包含不安全路径：{entry}"));
        }
    }
    let mut extract = Command::new("tar");
    extract.args(["-xzf"]).arg(archive).arg("-C").arg(staging);
    manager.inherit_into(&mut extract);
    let output = extract
        .output()
        .map_err(|error| format!("无法执行 tar：{error}"))?;
    if !output.status.success() {
        return Err(format!(
            "解压 Pi 运行时失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    if !pi_runtime_files_ready(staging) {
        return Err("Pi 运行时归档解压完成，但入口不完整".to_string());
    }
    Ok(())
}

#[cfg(any(test, not(debug_assertions)))]
fn list_archive_entries(
    archive: &Path,
    manager: &RuntimeManagerLock,
) -> Result<Vec<String>, String> {
    let mut command = Command::new("tar");
    command.arg("-tzf").arg(archive);
    manager.inherit_into(&mut command);
    let output = command
        .output()
        .map_err(|error| format!("无法执行 tar：{error}"))?;
    if !output.status.success() {
        return Err(format!(
            "列出 Pi 运行时归档失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let text =
        String::from_utf8(output.stdout).map_err(|_| "Pi 运行时归档清单不是 UTF-8".to_string())?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

/// Return the expected Pi entry and whether that runtime is currently usable. This never
/// downloads, unpacks, or waits for a manager transaction.
pub(crate) fn managed_pi_diagnostic() -> Result<(PathBuf, bool), String> {
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err("此平台没有 Smelt 受管 Bun".to_string())
    }
    #[cfg(all(debug_assertions, any(target_os = "macos", target_os = "linux")))]
    {
        let root = workspace_pi_runtime_root()?;
        let entry = root.join("src/main.ts");
        let store = GenerationStore::managed()?;
        let Some(_manager) = store.try_lock_manager()? else {
            return Ok((entry, false));
        };
        let bun = bun_manifest();
        let bun_root = store.generation_root(GenerationKind::Bun, &bun.generation_id);
        let ready = store.current_locked(GenerationKind::Bun).as_deref()
            == Some(bun.generation_id.as_str())
            && bun_tree_matches(&bun_root, &bun)
            && pi_runtime_files_ready(&root);
        Ok((entry, ready))
    }
    #[cfg(all(not(debug_assertions), any(target_os = "macos", target_os = "linux")))]
    {
        let store = GenerationStore::managed()?;
        let bun = bun_manifest();
        let pi = pi_manifest(&bun.generation_id, PI_RUNTIME_ARCHIVE_SHA256);
        let root = store.generation_root(GenerationKind::Pi, &pi.generation_id);
        let entry = root.join("src/main.ts");
        let Some(_manager) = store.try_lock_manager()? else {
            return Ok((entry, false));
        };
        let bun_root = store.generation_root(GenerationKind::Bun, &bun.generation_id);
        let ready = store.current_locked(GenerationKind::Bun).as_deref()
            == Some(bun.generation_id.as_str())
            && bun_tree_matches(&bun_root, &bun)
            && store.current_locked(GenerationKind::Pi).as_deref()
                == Some(pi.generation_id.as_str())
            && pi_tree_matches(&root, &pi);
        Ok((entry, ready))
    }
}

pub(crate) fn sync_managed_pi_agent(status: &dyn Fn(&str)) -> Result<ManagedPiRuntime, String> {
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = status;
        Err("受管 Pi 运行时只在 macOS 和 Linux 上提供".to_string())
    }
    #[cfg(all(debug_assertions, any(target_os = "macos", target_os = "linux")))]
    {
        sync_workspace_pi_agent(status)
    }
    #[cfg(all(not(debug_assertions), any(target_os = "macos", target_os = "linux")))]
    {
        sync_shipped_pi_agent(status)
    }
}

#[cfg(all(debug_assertions, any(target_os = "macos", target_os = "linux")))]
fn sync_workspace_pi_agent(status: &dyn Fn(&str)) -> Result<ManagedPiRuntime, String> {
    let store = GenerationStore::managed()?;
    let manager = store.lock_manager()?;
    let bun_runtime = sync_managed_bun_locked(&store, status, &manager)
        .map_err(|error| format!("无法准备 Smelt Pi 运行时：{error}"))?;
    let root = workspace_pi_runtime_root()?;
    if !pi_runtime_files_ready(&root) {
        return Err("开发构建使用仓库里的 pi-runtime/。请先运行 make pi-runtime-check".to_string());
    }
    drop(manager);
    Ok(ManagedPiRuntime {
        bun: bun_runtime.path.clone(),
        entry: root.join("src/main.ts"),
        root,
        bun_runtime,
        pi_lease: None,
    })
}

#[cfg(all(not(debug_assertions), any(target_os = "macos", target_os = "linux")))]
fn sync_shipped_pi_agent(status: &dyn Fn(&str)) -> Result<ManagedPiRuntime, String> {
    let store = GenerationStore::managed()?;
    let manager = store.lock_manager()?;
    let bun_runtime = sync_managed_bun_locked(&store, status, &manager)
        .map_err(|error| format!("无法准备 Smelt Pi 运行时：{error}"))?;
    let manifest = pi_manifest(&bun_runtime.generation_id, PI_RUNTIME_ARCHIVE_SHA256);
    store.remove_stale_staging_locked();
    let root = store.generation_root(GenerationKind::Pi, &manifest.generation_id);
    if !pi_tree_matches(&root, &manifest) {
        store.discard_invalid_locked(GenerationKind::Pi, &manifest.generation_id)?;
        status("正在解压 Pi 运行时（仅首次或版本变化）…");
        let archive = shipped_runtime_dir()?.join(shipped_pi_archive_name());
        store.publish_locked(
            GenerationKind::Pi,
            &manifest.generation_id,
            &manifest,
            |staging| install_pi_archive(staging, &archive, PI_RUNTIME_ARCHIVE_SHA256, &manager),
        )?;
    }
    let pi_lease = store.acquire_shared_locked(GenerationKind::Pi, &manifest.generation_id)?;
    store.write_current_locked(GenerationKind::Pi, &manifest.generation_id)?;
    let mut keep = HashSet::new();
    keep.insert(manifest.generation_id);
    store.gc_kind_locked(GenerationKind::Pi, &keep);
    drop(manager);
    Ok(ManagedPiRuntime {
        bun: bun_runtime.path.clone(),
        entry: root.join("src/main.ts"),
        root,
        bun_runtime,
        pi_lease: Some(pi_lease),
    })
}

pub(crate) fn sync_managed_pi_tool(
    relative: &str,
    status: &dyn Fn(&str),
) -> Result<(ManagedPiRuntime, PathBuf), String> {
    let runtime = sync_managed_pi_agent(status)?;
    let script = runtime.root.join(relative);
    if !script.is_file() {
        return Err(format!("Pi 运行时里没有 {relative}"));
    }
    Ok((runtime, script))
}

fn sha256_file(path: &Path) -> Result<String, String> {
    crate::daemon_image::content_fingerprint(path)
        .map_err(|error| format!("读取 {} 失败：{error}", path.display()))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn validate_bun(bun: &Path) -> Result<(), String> {
    let metadata =
        fs::symlink_metadata(bun).map_err(|error| format!("读取受管 Bun 元数据失败：{error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("受管 Bun 不是普通文件".to_string());
    }
    if metadata.permissions().mode() & 0o111 == 0 {
        return Err("受管 Bun 没有可执行权限".to_string());
    }
    let actual = sha256_file(bun)?;
    if actual != BUN_EXECUTABLE_SHA256 {
        return Err(format!(
            "受管 Bun 制品校验失败（期望 {BUN_EXECUTABLE_SHA256}，实际 {actual}）"
        ));
    }
    let output = Command::new(bun)
        .arg("--version")
        .output()
        .map_err(|error| format!("受管 Bun 无法执行：{error}"))?;
    if !output.status.success() || String::from_utf8_lossy(&output.stdout).trim() != BUN_VERSION {
        return Err("受管 Bun 版本检查失败".to_string());
    }
    Ok(())
}

pub(crate) fn pi_runtime_version() -> &'static str {
    PI_RUNTIME_VERSION
}

pub(crate) fn pi_runtime_prepare_hint() -> &'static str {
    #[cfg(debug_assertions)]
    {
        "请先运行 make pi-runtime-check"
    }
    #[cfg(not(debug_assertions))]
    {
        "首次使用解压随包运行时"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pi_runtime_version_tracks_the_workspace() {
        assert_eq!(pi_runtime_version(), env!("CARGO_PKG_VERSION"));
        let main = include_str!("../../../pi-runtime/src/main.ts");
        assert!(main.contains("registerBunOAuthFlows()"));
        let manifest = pi_manifest("bun-test", &"ab".repeat(32));
        let other = pi_manifest("bun-test", &"cd".repeat(32));
        assert_ne!(manifest.generation_id, other.generation_id);
        assert_eq!(manifest.runtime_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(manifest.archive_sha256, "ab".repeat(32));
        assert!(manifest.generation_id.starts_with("pi-"));
        assert!(manifest.generation_id.len() <= 96);
    }

    #[test]
    fn archive_entry_is_safe_rejects_escape_paths() {
        assert!(archive_entry_is_safe("src/main.ts"));
        assert!(archive_entry_is_safe("./src/main.ts"));
        assert!(archive_entry_is_safe(
            "node_modules/@earendil-works/pi-coding-agent/dist/modes/rpc/rpc-mode.js"
        ));
        assert!(!archive_entry_is_safe("../etc/passwd"));
        assert!(!archive_entry_is_safe("/etc/passwd"));
        assert!(!archive_entry_is_safe("src/../../etc/passwd"));
        assert!(!archive_entry_is_safe(""));
    }

    #[test]
    fn pi_archive_round_trip_publishes_a_verified_tree() {
        let home = std::env::temp_dir().join(format!(
            "smelt-pi-archive-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let tree = home.join("tree");
        fs::create_dir_all(tree.join("src")).unwrap();
        fs::write(tree.join("package.json"), "{}\n").unwrap();
        fs::write(tree.join("src/main.ts"), "export {}\n").unwrap();
        let bundle = tree.join("node_modules/@earendil-works/pi-coding-agent/dist/bundle");
        let rpc = tree.join("node_modules/@earendil-works/pi-coding-agent/dist/modes/rpc");
        fs::create_dir_all(&bundle).unwrap();
        fs::create_dir_all(&rpc).unwrap();
        fs::write(bundle.join("rpc-entry.js"), "ok\n").unwrap();
        fs::write(rpc.join("rpc-mode.js"), "ok\n").unwrap();

        let archive = home.join("pi-runtime.tar.gz");
        let packed = Command::new("tar")
            .args(["-czf"])
            .arg(&archive)
            .arg("-C")
            .arg(&tree)
            .args(["package.json", "src", "node_modules"])
            .status()
            .unwrap();
        assert!(packed.success());
        let sha = sha256_file(&archive).unwrap();

        let store = GenerationStore::new(home.join("runtime"));
        let manager = store.lock_manager().unwrap();
        let wrong = store.publish_locked(
            GenerationKind::Pi,
            "pi-wrong",
            &pi_manifest("bun-test", &sha),
            |staging| install_pi_archive(staging, &archive, &"0".repeat(64), &manager),
        );
        assert!(wrong.unwrap_err().contains("Pi 运行时归档校验失败"));
        assert!(
            !store
                .generation_root(GenerationKind::Pi, "pi-wrong")
                .exists()
        );

        let hostile = home.join("hostile.tar.gz");
        fs::write(&hostile, gzip_bytes(&ustar_bytes("../evil"))).unwrap();
        let hostile_error = install_pi_archive(
            &home.join("hostile-staging"),
            &hostile,
            &sha256_file(&hostile).unwrap(),
            &manager,
        )
        .unwrap_err();
        assert!(
            hostile_error.contains("不安全路径")
                || hostile_error.contains("列出 Pi 运行时归档失败"),
            "{hostile_error}"
        );

        let manifest = pi_manifest("bun-test", &sha);
        let root = store
            .publish_locked(
                GenerationKind::Pi,
                &manifest.generation_id,
                &manifest,
                |staging| install_pi_archive(staging, &archive, &sha, &manager),
            )
            .unwrap();
        assert!(pi_tree_matches(&root, &manifest));
        assert_eq!(
            fs::read_to_string(root.join("src/main.ts")).unwrap(),
            "export {}\n"
        );
        drop(manager);
        let _ = fs::remove_dir_all(home);
    }

    fn ustar_bytes(name: &str) -> Vec<u8> {
        let mut header = [0u8; 512];
        let name_bytes = name.as_bytes();
        assert!(name_bytes.len() <= 100);
        header[..name_bytes.len()].copy_from_slice(name_bytes);
        write_octal(&mut header, 100, 8, 0o644);
        write_octal(&mut header, 108, 8, 0);
        write_octal(&mut header, 116, 8, 0);
        write_octal(&mut header, 124, 12, 0);
        write_octal(&mut header, 136, 12, 0);
        header[148..156].fill(b' ');
        header[156] = b'0';
        header[257..262].copy_from_slice(b"ustar");
        header[263..265].copy_from_slice(b"00");
        let sum: u32 = header.iter().map(|byte| u32::from(*byte)).sum();
        let checksum = format!("{sum:06o}\0 ");
        header[148..156].copy_from_slice(checksum.as_bytes());
        let mut bytes = header.to_vec();
        bytes.extend_from_slice(&[0u8; 1024]);
        bytes
    }

    fn write_octal(header: &mut [u8], offset: usize, width: usize, value: u64) {
        let rendered = format!("{value:0width$o}\0", width = width - 1);
        header[offset..offset + width].copy_from_slice(rendered.as_bytes());
    }

    fn gzip_bytes(bytes: &[u8]) -> Vec<u8> {
        let mut child = Command::new("gzip")
            .arg("-c")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write as _;
        child.stdin.take().unwrap().write_all(bytes).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        output.stdout
    }

    #[test]
    fn bun_launcher_follows_the_latest_generation() {
        let runtime =
            std::env::temp_dir().join(format!("smelt-bun-launcher-{}", uuid::Uuid::new_v4()));
        let first = runtime.join("store/bun/bun-a/bun");
        let second = runtime.join("store/bun/bun-b/bun");

        publish_bun_launcher(&runtime, &first).unwrap();
        assert_eq!(fs::read_link(runtime.join(BUN_LAUNCHER)).unwrap(), first);
        publish_bun_launcher(&runtime, &first).unwrap();
        publish_bun_launcher(&runtime, &second).unwrap();
        assert_eq!(fs::read_link(runtime.join(BUN_LAUNCHER)).unwrap(), second);
        assert!(
            MANAGED_BUN_LAUNCHER_HOME_PATH.ends_with(BUN_LAUNCHER),
            "写进外部配置的路径必须就是这个入口"
        );
        let leftovers = fs::read_dir(runtime.join("bin")).unwrap().count();
        assert_eq!(leftovers, 1, "临时链接不能留下");
        fs::remove_dir_all(runtime).ok();
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn bun_manifest_pins_the_release_and_executable() {
        let manifest = bun_manifest();
        assert_eq!(manifest.version, BUN_VERSION);
        assert_eq!(manifest.executable_sha256.len(), 64);
        assert!(
            manifest
                .download_url
                .contains(&format!("/bun-v{BUN_VERSION}/"))
        );
        assert_eq!(manifest.archive_sha256.len(), 64);
    }

    /// 用已经打好的随包归档和 Bun zip 走解压发布，再离线跑一次 `get_state`。
    /// 不走 debug 的仓库目录。准备：`make pi-runtime-pack`，然后
    /// `cargo test -p smelt-core unpacks_the_shipped_pi_archive_offline -- --ignored`。
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    #[ignore = "需要 make pi-runtime-pack 产生的归档和 Bun zip"]
    fn unpacks_the_shipped_pi_archive_offline() {
        let archive = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../../pi-runtime/dist/{}",
            shipped_pi_archive_name()
        ));
        let zip = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.cache/bun")
            .join(shipped_bun_archive_name());
        assert!(
            archive.is_file(),
            "缺少 {}。先运行 make pi-runtime-pack",
            archive.display()
        );
        assert!(
            zip.is_file(),
            "缺少 {}。先运行 ./scripts/bun.sh",
            zip.display()
        );
        let sha = sha256_file(&archive).unwrap();
        let home =
            std::env::temp_dir().join(format!("smelt-pi-unpack-{}", uuid::Uuid::new_v4().simple()));
        let store = GenerationStore::new(home.join("runtime"));
        let manager = store.lock_manager().unwrap();
        let bun = bun_manifest();
        let bun_root = store
            .publish_locked(GenerationKind::Bun, &bun.generation_id, &bun, |staging| {
                install_bun_from_zip(staging, &zip, BUN_DOWNLOAD.1, &manager)
            })
            .unwrap();
        let manifest = pi_manifest(&bun.generation_id, &sha);
        let root = store
            .publish_locked(
                GenerationKind::Pi,
                &manifest.generation_id,
                &manifest,
                |staging| install_pi_archive(staging, &archive, &sha, &manager),
            )
            .unwrap();
        assert!(pi_tree_matches(&root, &manifest));
        drop(manager);

        let pi_home = home.join("pi-agent");
        fs::create_dir_all(&pi_home).unwrap();
        let mut child = Command::new(bun_root.join("bun"))
            .arg(root.join("src/main.ts"))
            .args(["--offline", "--no-approve"])
            .current_dir(&root)
            .env("HOME", &home)
            .env("PI_CODING_AGENT_DIR", &pi_home)
            .env("HTTP_PROXY", "http://127.0.0.1:9")
            .env("HTTPS_PROXY", "http://127.0.0.1:9")
            .env("ALL_PROXY", "http://127.0.0.1:9")
            .env("http_proxy", "http://127.0.0.1:9")
            .env("https_proxy", "http://127.0.0.1:9")
            .env("all_proxy", "http://127.0.0.1:9")
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write as _;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"{\"id\":\"production-state\",\"type\":\"get_state\"}\n")
            .unwrap();
        let pid = child.id();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(child.wait_with_output());
        });
        let output = match rx.recv_timeout(std::time::Duration::from_secs(20)) {
            Ok(output) => output.unwrap(),
            Err(_) => {
                unsafe { libc::kill(pid as i32, libc::SIGKILL) };
                panic!("离线 get_state 超时");
            }
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "exit {:?}\n{stdout}\n{stderr}",
            output.status.code()
        );
        assert!(
            stdout.lines().any(|line| {
                let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                    return false;
                };
                value["id"] == "production-state"
                    && value["type"] == "response"
                    && value["success"] == true
            }),
            "{stdout}\n{stderr}"
        );
        let _ = fs::remove_dir_all(home);
    }
}
