//! Provisioning for Smelt's immutable Bun and Pi runtime generations.

#![cfg(unix)]

use crate::runtime_generation::{
    BunGenerationManifest, GENERATION_SCHEMA, GenerationKind, GenerationLease, GenerationStore,
    PiGenerationManifest, inherit_generation_fds,
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs,
    io::Read as _,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

pub(crate) const BUN_VERSION: &str = "1.4.0";
pub(crate) const PI_BRIDGE_PROTOCOL: &str = "smelt-pi-rpc-v1";
include!(concat!(env!("OUT_DIR"), "/pi_agent_runtime_files.rs"));

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
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const BUN_ZIP_DIR: &str = "bun-darwin-aarch64";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub(crate) const BUN_EXECUTABLE_SHA256: &str =
    "539598c775882420b9d8deb7dc14d845f20f7d26f5600c50ab067dde6ac3f3bf";
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const BUN_ZIP_DIR: &str = "bun-darwin-x64";
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub(crate) const BUN_EXECUTABLE_SHA256: &str =
    "ca8a18d0116d7b6b19f53bb0d8c48e487c0757cab4dc3f4f8cc5e43a44cd75d8";

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
    pi_lease: GenerationLease,
}

impl ManagedPiRuntime {
    pub(crate) fn process_args(&self, trailing: impl IntoIterator<Item = String>) -> Vec<String> {
        std::iter::once(self.bun.to_string_lossy().into_owned())
            .chain(std::iter::once(self.entry.to_string_lossy().into_owned()))
            .chain(trailing)
            .collect()
    }

    pub(crate) fn inherit_into(&self, command: &mut Command) {
        inherit_generation_fds(
            command,
            &[self.bun_runtime.lease.clone(), self.pi_lease.clone()],
        );
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

pub(crate) fn pi_agent_dependency_fingerprint() -> String {
    sha256_parts(
        PI_AGENT_RUNTIME_FILES
            .iter()
            .filter(|(name, _)| {
                *name == "package.json" || *name == "bun.lock" || name.starts_with("patches/")
            })
            .flat_map(|(name, content)| [name.as_bytes(), content.as_bytes()]),
    )
}

fn pi_agent_source_fingerprint() -> String {
    sha256_parts(
        PI_AGENT_RUNTIME_FILES
            .iter()
            .flat_map(|(name, content)| [name.as_bytes(), content.as_bytes()]),
    )
}

fn pi_manifest(bun_id: &str) -> PiGenerationManifest {
    let source_sha256 = pi_agent_source_fingerprint();
    let dependency_sha256 = pi_agent_dependency_fingerprint();
    let identity = sha256_parts([
        PI_AGENT_RUNTIME_VERSION.as_bytes(),
        source_sha256.as_bytes(),
        dependency_sha256.as_bytes(),
        std::env::consts::OS.as_bytes(),
        std::env::consts::ARCH.as_bytes(),
        PI_BRIDGE_PROTOCOL.as_bytes(),
        bun_id.as_bytes(),
    ]);
    PiGenerationManifest {
        schema: GENERATION_SCHEMA,
        generation_id: format!("pi-{identity}"),
        runtime_version: PI_AGENT_RUNTIME_VERSION.to_string(),
        platform: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        bridge_protocol: PI_BRIDGE_PROTOCOL.to_string(),
        source_sha256,
        dependency_sha256,
        bun_generation_id: bun_id.to_string(),
    }
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

fn pi_tree_matches(root: &Path, expected: &PiGenerationManifest) -> bool {
    root.join("src/main.ts").is_file()
        && managed_pi_agent_dependencies_installed(root)
        && fs::read(root.join("manifest.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<PiGenerationManifest>(&bytes).ok())
            .as_ref()
            == Some(expected)
}

#[cfg(not(target_os = "macos"))]
pub fn sync_managed_bun(_status: &dyn Fn(&str)) -> Result<ManagedBunRuntime, String> {
    Err("受管 Bun 运行时只在 macOS 上提供".to_string())
}

#[cfg(target_os = "macos")]
pub fn sync_managed_bun(status: &dyn Fn(&str)) -> Result<ManagedBunRuntime, String> {
    let store = GenerationStore::managed()?;
    let manager = store.lock_manager()?;
    let runtime = sync_managed_bun_locked(&store, status)?;
    drop(manager);
    Ok(runtime)
}

#[cfg(target_os = "macos")]
fn sync_managed_bun_locked(
    store: &GenerationStore,
    status: &dyn Fn(&str),
) -> Result<ManagedBunRuntime, String> {
    let manifest = bun_manifest();
    store.remove_stale_staging_locked();
    let root = store.generation_root(GenerationKind::Bun, &manifest.generation_id);
    if !bun_tree_matches(&root, &manifest) {
        store.discard_invalid_locked(GenerationKind::Bun, &manifest.generation_id)?;
        status("正在下载 Bun 运行时（约 25MB，仅首次）…");
        store.publish_locked(
            GenerationKind::Bun,
            &manifest.generation_id,
            &manifest,
            |staging| install_bun_generation(staging, status),
        )?;
    }
    let lease = store.acquire_shared_locked(GenerationKind::Bun, &manifest.generation_id)?;
    store.write_current_locked(GenerationKind::Bun, &manifest.generation_id)?;
    let mut keep = HashSet::new();
    keep.insert(manifest.generation_id.clone());
    store.gc_kind_locked(GenerationKind::Bun, &keep);
    Ok(ManagedBunRuntime {
        path: root.join("bun"),
        generation_id: manifest.generation_id,
        lease,
    })
}

#[cfg(target_os = "macos")]
fn install_bun_generation(staging: &Path, status: &dyn Fn(&str)) -> Result<(), String> {
    let (url, expected_archive_sha) = BUN_DOWNLOAD;
    let archive = staging.join("download.zip");
    let output = Command::new("curl")
        .args(["-fsSL", "--retry", "2", "-o"])
        .arg(&archive)
        .arg(url)
        .output()
        .map_err(|error| format!("无法执行 curl：{error}"))?;
    if !output.status.success() {
        return Err(format!(
            "下载 Bun 失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let actual_archive_sha = sha256_file(&archive)?;
    if actual_archive_sha != expected_archive_sha {
        return Err(format!(
            "Bun 下载校验失败（期望 {expected_archive_sha}，实际 {actual_archive_sha}）"
        ));
    }
    status("校验并解压 Bun 运行时…");
    let unpack = staging.join("unpack");
    fs::create_dir(&unpack).map_err(|error| format!("创建 Bun 解压目录失败：{error}"))?;
    let output = Command::new("unzip")
        .args(["-q"])
        .arg(&archive)
        .arg("-d")
        .arg(&unpack)
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
    fs::remove_file(&archive).map_err(|error| format!("清理 Bun 下载包失败：{error}"))?;
    fs::remove_dir_all(&unpack).map_err(|error| format!("清理 Bun 解压目录失败：{error}"))?;
    validate_bun(&bun)
}

pub fn try_current_managed_bun() -> Option<ManagedBunRuntime> {
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
    #[cfg(target_os = "macos")]
    {
        let store = GenerationStore::managed().ok()?;
        let manager = store.try_lock_manager().ok()??;
        let expected = bun_manifest();
        let current = store.current_locked(GenerationKind::Bun)?;
        if current != expected.generation_id {
            return None;
        }
        let root = store.generation_root(GenerationKind::Bun, &current);
        if !bun_tree_matches(&root, &expected) {
            return None;
        }
        let lease = store
            .acquire_shared_locked(GenerationKind::Bun, &current)
            .ok()?;
        drop(manager);
        Some(ManagedBunRuntime {
            path: root.join("bun"),
            generation_id: current,
            lease,
        })
    }
}

pub fn managed_bun_path_if_ready() -> Option<PathBuf> {
    try_current_managed_bun().map(|runtime| runtime.path)
}

/// Return the expected Pi entry and whether the exact Bun+Pi generation pair is currently
/// published. This never downloads, installs, hashes the Bun executable, or waits for a manager
/// transaction; it is safe for diagnostics and availability UI.
pub(crate) fn managed_pi_diagnostic() -> Result<(PathBuf, bool), String> {
    #[cfg(not(target_os = "macos"))]
    {
        Err("此平台没有 Smelt 受管 Bun".to_string())
    }
    #[cfg(target_os = "macos")]
    {
        let store = GenerationStore::managed()?;
        let bun = bun_manifest();
        let pi = pi_manifest(&bun.generation_id);
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
    #[cfg(not(target_os = "macos"))]
    {
        return Err("受管 Pi 运行时只在 macOS 上提供".to_string());
    }
    #[cfg(target_os = "macos")]
    let store = GenerationStore::managed()?;
    #[cfg(target_os = "macos")]
    let manager = store.lock_manager()?;
    #[cfg(target_os = "macos")]
    let bun_runtime = sync_managed_bun_locked(&store, status)
        .map_err(|error| format!("无法准备 Smelt Pi 运行时：{error}"))?;
    #[cfg(target_os = "macos")]
    let manifest = pi_manifest(&bun_runtime.generation_id);
    store.remove_stale_staging_locked();
    let root = store.generation_root(GenerationKind::Pi, &manifest.generation_id);
    if !pi_tree_matches(&root, &manifest) {
        store.discard_invalid_locked(GenerationKind::Pi, &manifest.generation_id)?;
        status("正在准备 Pi 智能体运行时（仅首次或版本变化）…");
        store.publish_locked(
            GenerationKind::Pi,
            &manifest.generation_id,
            &manifest,
            |staging| {
                materialize_pi_files(staging)?;
                run_pi_install(&bun_runtime.path, staging)?;
                if !managed_pi_agent_dependencies_installed(staging) {
                    return Err("Pi 依赖安装完成，但必要的 RPC 补丁或入口不完整".to_string());
                }
                Ok(())
            },
        )?;
    }
    let pi_lease = store.acquire_shared_locked(GenerationKind::Pi, &manifest.generation_id)?;
    store.write_current_locked(GenerationKind::Pi, &manifest.generation_id)?;
    let mut keep = HashSet::new();
    keep.insert(manifest.generation_id.clone());
    store.gc_kind_locked(GenerationKind::Pi, &keep);
    drop(manager);
    Ok(ManagedPiRuntime {
        bun: bun_runtime.path.clone(),
        entry: root.join("src/main.ts"),
        root,
        bun_runtime,
        pi_lease,
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

fn materialize_pi_files(root: &Path) -> Result<(), String> {
    for (relative, content) in PI_AGENT_RUNTIME_FILES {
        let path = root.join(relative);
        let parent = path.parent().ok_or("Pi runtime 文件没有父目录")?;
        fs::create_dir_all(parent).map_err(|error| format!("创建 Pi runtime 目录失败：{error}"))?;
        fs::write(&path, content)
            .map_err(|error| format!("写入 Pi runtime 文件 {} 失败：{error}", path.display()))?;
    }
    Ok(())
}

fn run_pi_install(bun: &Path, root: &Path) -> Result<(), String> {
    const TIMEOUT: Duration = Duration::from_secs(15 * 60);
    let mut child = Command::new(bun)
        .args([
            "install",
            "--frozen-lockfile",
            "--production",
            "--ignore-scripts",
        ])
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .env("NO_COLOR", "1")
        .spawn()
        .map_err(|error| format!("无法启动 Pi 运行时依赖安装：{error}"))?;
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("Pi 运行时依赖安装失败（{status}）")),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(200)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Pi 运行时依赖安装超时（15 分钟）".to_string());
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("等待 Pi 运行时依赖安装失败：{error}"));
            }
        }
    }
}

fn managed_pi_agent_dependencies_installed(root: &Path) -> bool {
    let rpc_entry =
        root.join("node_modules/@earendil-works/pi-coding-agent/dist/bundle/rpc-entry.js");
    let rpc_mode =
        root.join("node_modules/@earendil-works/pi-coding-agent/dist/modes/rpc/rpc-mode.js");
    rpc_entry.is_file()
        && fs::read_to_string(rpc_mode)
            .is_ok_and(|source| pi_rpc_duplicate_rebind_patch_is_applied(&source))
}

pub(crate) fn pi_rpc_duplicate_rebind_patch_is_applied(source: &str) -> bool {
    ["new_session", "switch_session", "fork", "clone"]
        .into_iter()
        .all(|command| {
            let marker = format!("case \"{command}\": {{");
            let Some(start) = source.find(&marker) else {
                return false;
            };
            let after_marker = &source[start + marker.len()..];
            let end = after_marker
                .find("\n            case \"")
                .unwrap_or(after_marker.len());
            !after_marker[..end].contains("await rebindSession()")
        })
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file =
        fs::File::open(path).map_err(|error| format!("读取 {} 失败：{error}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("读取 {} 失败：{error}", path.display()))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(target_os = "macos")]
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
    PI_AGENT_RUNTIME_VERSION
}

pub(crate) fn pi_package_json() -> &'static str {
    PI_AGENT_PACKAGE_JSON
}

pub(crate) fn pi_runtime_files() -> &'static [(&'static str, &'static str)] {
    PI_AGENT_RUNTIME_FILES
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect_runtime_files(dir: &Path, package: &Path, output: &mut Vec<String>) {
        for entry in fs::read_dir(dir).expect("读取 runtime 源码目录") {
            let path = entry.expect("读取 runtime 源码项").path();
            if path.is_dir() {
                collect_runtime_files(&path, package, output);
                continue;
            }
            let relative = path
                .strip_prefix(package)
                .expect("runtime 文件应位于 package 下")
                .to_string_lossy()
                .replace('\\', "/");
            if (relative.starts_with("src/")
                && relative.ends_with(".ts")
                && !relative.ends_with(".d.ts"))
                || relative.starts_with("patches/")
            {
                output.push(relative);
            }
        }
    }

    #[test]
    fn embedded_runtime_files_match_the_source_tree() {
        let package = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/pi-agent");
        let mut expected = vec!["bun.lock".to_string(), "package.json".to_string()];
        collect_runtime_files(&package.join("src"), &package, &mut expected);
        collect_runtime_files(&package.join("patches"), &package, &mut expected);
        expected.sort();

        let mut embedded = pi_runtime_files()
            .iter()
            .map(|(relative, _)| (*relative).to_string())
            .collect::<Vec<_>>();
        embedded.sort();
        assert_eq!(embedded, expected);

        let root = std::env::temp_dir().join(format!("smelt-pi-stage-{}", uuid::Uuid::new_v4()));
        materialize_pi_files(&root).expect("物化 Pi runtime staging");
        for (relative, content) in pi_runtime_files() {
            assert_eq!(fs::read(root.join(relative)).unwrap(), content.as_bytes());
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn embedded_package_version_and_oauth_registration_are_present() {
        let manifest: serde_json::Value = serde_json::from_str(pi_package_json()).unwrap();
        assert_eq!(manifest["version"], pi_runtime_version());
        let main = include_str!("../../../packages/pi-agent/src/main.ts");
        assert!(main.contains("registerBunOAuthFlows()"));
    }

    #[test]
    fn rpc_patch_postcondition_covers_every_session_replacement_handler() {
        let clean = ["new_session", "switch_session", "fork", "clone"]
            .into_iter()
            .map(|name| format!("case \"{name}\": {{\n return success();\n }}\n"))
            .collect::<String>();
        assert!(pi_rpc_duplicate_rebind_patch_is_applied(&clean));
        for name in ["new_session", "switch_session", "fork", "clone"] {
            let drifted = clean.replacen(
                &format!("case \"{name}\": {{"),
                &format!("case \"{name}\": {{\n await rebindSession();"),
                1,
            );
            assert!(!pi_rpc_duplicate_rebind_patch_is_applied(&drifted));
        }
        assert!(!pi_rpc_duplicate_rebind_patch_is_applied(&clean.replacen(
            "case \"clone\": {",
            "case \"removed\": {",
            1,
        )));
    }

    #[cfg(target_os = "macos")]
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
}
