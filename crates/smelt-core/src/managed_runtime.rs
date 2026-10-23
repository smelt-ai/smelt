//! 受管 Bun 仍按 generation 下载到 `~/.smelt/runtime`。
//! Pi 不再物化源码：跑的是 `~/.smelt/bin/smelt-pi` 指向的那份编译好的 smelt-pi（见 `pi_image`）。
//!
//! Bun 只留给 MCP 预设的 `bun x` 和 Claude/Codex 的 `bunx`。`~/.smelt/runtime/bin/bun`
//! 指向当前 generation，`mcp.json` 里写的还是这条路径。

#![cfg(unix)]

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

pub(crate) const BUN_VERSION: &str = "1.4.3";

#[cfg(any(target_os = "macos", target_os = "linux", test))]
/// 当前受管 Bun 的固定入口，相对 runtime 根目录。generation 目录名带哈希、随版本变，
/// 写进外部配置（如 `mcp.json` 的 `command`）的路径必须是这一条。
const BUN_LAUNCHER: &str = "bin/bun";

pub use smelt_model::agent_paths::MANAGED_BUN_LAUNCHER_HOME_PATH;

include!(concat!(env!("OUT_DIR"), "/pi_agent_version.rs"));

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const BUN_DOWNLOAD: (&str, &str) = (
    "https://github.com/oven-sh/bun/releases/download/bun-v1.4.3/bun-darwin-aarch64.zip",
    "80afd4c069b46a6fa8f3ec520bebc750a7843e4a8d5b9583d29ad436c00a3403",
);
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const BUN_DOWNLOAD: (&str, &str) = (
    "https://github.com/oven-sh/bun/releases/download/bun-v1.4.3/bun-darwin-x64.zip",
    "62660e6438eb872278877acca77e1152d5d031324df368d4a81b428211489d2d",
);
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const BUN_ZIP_DIR: &str = "bun-darwin-aarch64";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub(crate) const BUN_EXECUTABLE_SHA256: &str =
    "28c7dd0a06916aba284d570ef44a2d70e9c538eaf2ba58dfe0910525563b3f79";
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const BUN_ZIP_DIR: &str = "bun-darwin-x64";
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub(crate) const BUN_EXECUTABLE_SHA256: &str =
    "c2a945a9d33f9088b32ac3550d1d1fd485a7fc10d71c66f91ef0d33b41cefa7c";
// Linux x64 用 baseline 版：不要求 AVX2，云主机的老 CPU 也能跑。
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const BUN_DOWNLOAD: (&str, &str) = (
    "https://github.com/oven-sh/bun/releases/download/bun-v1.4.3/bun-linux-x64-baseline.zip",
    "1fc2edac843102909e3a1be1d8d9802cc6071cf074e67e88231f4ffe0f8b397b",
);
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const BUN_ZIP_DIR: &str = "bun-linux-x64-baseline";
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub(crate) const BUN_EXECUTABLE_SHA256: &str =
    "7ce7d6b654eddeec20afddf2396317fba6dc19ab52788a27e894e1806bf8f24b";
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const BUN_DOWNLOAD: (&str, &str) = (
    "https://github.com/oven-sh/bun/releases/download/bun-v1.4.3/bun-linux-aarch64.zip",
    "efa9813da5ed72423bf847f916e8d2c47c0d776add972354026a75e10da9aa21",
);
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const BUN_ZIP_DIR: &str = "bun-linux-aarch64";
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub(crate) const BUN_EXECUTABLE_SHA256: &str =
    "029ece746ac9f970c76d8f48bdd1b1d0c52f3636b74d19f546f5e7c1165c0abe";

/// npmmirror 按 GitHub Release 原样同步的 Bun 包，连不上 GitHub 时从这里下。
#[cfg(any(target_os = "macos", target_os = "linux"))]
const BUN_MIRROR: &str = "https://registry.npmmirror.com/-/binary/bun";

/// 和 `BUN_DOWNLOAD` 同一个包。校验和写死在上面，从哪下都一样可信，所以镜像地址不进
/// generation 身份：换着来源下，装的仍是同一个 generation。
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn bun_mirror_url() -> String {
    format!("{BUN_MIRROR}/bun-v{BUN_VERSION}/{BUN_ZIP_DIR}.zip")
}

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

/// 起 smelt-pi 时用这个环境变量告诉它守护这边的约定版本（`PI_SMELT_PROTOCOL`）。
/// smelt-pi 和守护分开发布，对不上它会拒绝启动，并在 stderr 说明。
pub(crate) const PI_PROTOCOL_ENV: &str = "SMELT_PI_PROTOCOL";

pub(crate) fn pi_protocol() -> String {
    PI_SMELT_PROTOCOL.to_string()
}

#[derive(Clone, Debug)]
pub(crate) struct ManagedPiRuntime {
    pub executable: PathBuf,
}

impl ManagedPiRuntime {
    pub(crate) fn process_args(&self, trailing: impl IntoIterator<Item = String>) -> Vec<String> {
        std::iter::once(self.executable.to_string_lossy().into_owned())
            .chain(trailing)
            .collect()
    }

    pub(crate) fn command(&self, subcommand: Option<&str>) -> Command {
        let mut command = Command::new(&self.executable);
        if let Some(subcommand) = subcommand {
            command.arg(subcommand);
        }
        command.env(PI_PROTOCOL_ENV, pi_protocol());
        command
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

#[cfg(any(target_os = "macos", target_os = "linux"))]
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

#[cfg(any(target_os = "macos", target_os = "linux"))]
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

/// 旧版把 Pi 源码连整份 node_modules 物化在 `runtime/store/pi` 下，现在跑的是编译好的 smelt-pi。
const RETIRED_PI_GENERATIONS: &str = "pi";

/// 守护启动后在后台调一次，删掉旧的 Pi 源码 generation。别的进程正管着 runtime 目录就
/// 这次不做；交接继承下来的旧 Pi 进程还占着的，留到它退出后的下一次启动。
pub fn retire_pi_source_generations() {
    let Ok(store) = GenerationStore::managed() else {
        return;
    };
    let Ok(Some(_manager)) = store.try_lock_manager() else {
        return;
    };
    store.retire_kind_locked(RETIRED_PI_GENERATIONS);
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
        status("正在下载 Bun 运行时（约 25MB，仅首次）…");
        store.publish_locked(
            GenerationKind::Bun,
            &manifest.generation_id,
            &manifest,
            |staging| install_bun_generation(staging, status, manager),
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

/// 下载 Bun 的 curl。连不上、或者连上了却半天不动（走代理、跨地区连 GitHub 常见）都要
/// 放弃，不能一直等：没有超时的话，第一次用 Pi 的对话会一直停在「正在想」，还占着准备
/// 运行时的锁，后面的对话也跟着等。两种超时 curl 都当可重试的错误，重试完仍失败就报出来。
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn bun_download_command(archive: &Path, url: &str) -> Command {
    let mut download = Command::new("curl");
    download
        .args(["-fsSL", "--retry", "2", "--connect-timeout", "20"])
        // 连续 60 秒低于 1 KB/s 就当卡住了。
        .args(["--speed-limit", "1024", "--speed-time", "60", "-o"])
        .arg(archive)
        .arg(url);
    download
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn install_bun_generation(
    staging: &Path,
    status: &dyn Fn(&str),
    manager: &RuntimeManagerLock,
) -> Result<(), String> {
    let (url, expected_archive_sha) = BUN_DOWNLOAD;
    let mirror = bun_mirror_url();
    // 连不上 GitHub（中国大陆常见）就先走镜像，省得等 GitHub 那边一轮轮超时。
    let sources = if crate::mirrors::reachable("github.com", 443) {
        [url, mirror.as_str()]
    } else {
        [mirror.as_str(), url]
    };
    let archive = staging.join("download.zip");
    let mut failures = Vec::new();
    for source in sources {
        let mut download = bun_download_command(&archive, source);
        manager.inherit_into(&mut download);
        let output = download
            .output()
            .map_err(|error| format!("无法执行 curl：{error}"))?;
        if output.status.success() {
            break;
        }
        failures.push(format!(
            "{source}：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    if failures.len() == sources.len() {
        return Err(format!("下载 Bun 失败：{}", failures.join("；")));
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
    let mut unzip = Command::new("unzip");
    unzip.args(["-q"]).arg(&archive).arg("-d").arg(&unpack);
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
    fs::remove_file(&archive).map_err(|error| format!("清理 Bun 下载包失败：{error}"))?;
    fs::remove_dir_all(&unpack).map_err(|error| format!("清理 Bun 解压目录失败：{error}"))?;
    validate_bun(&bun)
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
    PI_AGENT_RUNTIME_VERSION
}

/// 定位编译好的 `smelt-pi`。不下载、不写 `settings.json`。
pub(crate) fn sync_managed_pi_agent(_status: &dyn Fn(&str)) -> Result<ManagedPiRuntime, String> {
    let executable = locate_smelt_pi()?;
    require_photon_wasm(&executable)?;
    Ok(ManagedPiRuntime { executable })
}

/// 当前这一套 smelt-pi 里的内置技能目录。还没装 smelt-pi 时没有。
pub(crate) fn builtin_skills_dir() -> Option<PathBuf> {
    let executable = locate_smelt_pi().ok()?;
    Some(executable.parent()?.join(crate::pi_image::SKILLS_DIR))
}

/// 诊断用：不下载、不写启动器。二进制在且旁边有 photon wasm 才算就绪。
pub(crate) fn managed_pi_diagnostic() -> Result<(PathBuf, bool), String> {
    let executable = locate_smelt_pi()?;
    Ok((executable.clone(), photon_wasm(&executable).is_file()))
}

fn locate_smelt_pi() -> Result<PathBuf, String> {
    let home = smelt_paths::smelt_home().ok_or("找不到 home 目录")?;
    locate_smelt_pi_in(&home.join("bin"))
}

fn locate_smelt_pi_in(bin: &Path) -> Result<PathBuf, String> {
    crate::pi_image::current(bin).ok_or_else(|| {
        format!(
            "找不到 smelt-pi（{}）。桌面重新打开 Smelt 会从 App 里装上；Linux 上运行时启动时会按自己的版本补上，或重新运行安装脚本。",
            bin.join(crate::pi_image::LAUNCH_NAME).display()
        )
    })
}

fn photon_wasm(executable: &Path) -> PathBuf {
    executable.with_file_name("photon_rs_bg.wasm")
}

fn require_photon_wasm(executable: &Path) -> Result<(), String> {
    let wasm = photon_wasm(executable);
    if wasm.is_file() {
        Ok(())
    } else {
        Err(format!(
            "smelt-pi 旁边缺少 photon_rs_bg.wasm（{}）",
            wasm.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn executable(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, bytes).unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    #[test]
    fn process_args_are_the_binary_then_the_existing_flags() {
        let runtime = ManagedPiRuntime {
            executable: PathBuf::from("/Applications/Smelt.app/Contents/MacOS/smelt-pi"),
        };
        assert_eq!(
            runtime.process_args(["--offline".into(), "--no-approve".into()]),
            vec![
                "/Applications/Smelt.app/Contents/MacOS/smelt-pi",
                "--offline",
                "--no-approve",
            ]
        );
        assert_eq!(
            runtime.command(Some("auth")).get_program(),
            "/Applications/Smelt.app/Contents/MacOS/smelt-pi"
        );
        let args = runtime
            .command(Some("auth"))
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(args, vec!["auth".to_string()]);
        let command = runtime.command(Some("auth"));
        let protocol = command
            .get_envs()
            .find(|(name, _)| *name == PI_PROTOCOL_ENV)
            .and_then(|(_, value)| value);
        assert_eq!(
            protocol.map(|value| value.to_string_lossy().into_owned()),
            Some(pi_protocol()),
            "子命令也要带约定版本，不然 smelt-pi 会拒绝启动"
        );
    }

    #[test]
    fn smelt_pi_resolves_through_the_launch_link_to_its_real_path() {
        let root = std::env::temp_dir().join(format!(
            "smelt-pi-locate-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let error = locate_smelt_pi_in(&bin).expect_err("没发布过必须失败");
        assert!(error.contains("找不到 smelt-pi"), "实际错误：{error}");

        let published = bin.join(format!("smelt-pi-{}", "ab".repeat(32))).join("smelt-pi");
        executable(&published, b"pi");
        std::os::unix::fs::symlink(
            published.strip_prefix(&bin).unwrap(),
            bin.join("smelt-pi"),
        )
        .unwrap();
        assert_eq!(
            locate_smelt_pi_in(&bin).unwrap(),
            fs::canonicalize(&published).unwrap(),
            "要 exec 解析后的真实路径，链接换了也不影响已经起来的 Pi"
        );
        fs::remove_dir_all(root).ok();
    }

    /// 复现：远程 Linux 上下载 Bun 卡住，curl 没有超时，对话一直停在「正在想」。
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_stalled_bun_download_gives_up_instead_of_waiting_forever() {
        let command =
            bun_download_command(Path::new("/tmp/bun.zip"), "https://example.com/bun.zip");
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let value_of = |flag: &str| {
            args.iter()
                .position(|arg| arg == flag)
                .and_then(|index| args.get(index + 1))
                .map(String::as_str)
        };
        assert_eq!(value_of("--connect-timeout"), Some("20"));
        assert_eq!(value_of("--speed-limit"), Some("1024"));
        assert_eq!(value_of("--speed-time"), Some("60"));
        assert_eq!(value_of("--retry"), Some("2"));
        assert_eq!(value_of("-o"), Some("/tmp/bun.zip"));
        assert_eq!(
            args.last().map(String::as_str),
            Some("https://example.com/bun.zip")
        );
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

    #[test]
    fn photon_wasm_must_sit_beside_the_binary() {
        let dir = std::env::temp_dir().join(format!("smelt-pi-wasm-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let executable = dir.join("smelt-pi");
        fs::write(&executable, b"pi").unwrap();
        assert!(require_photon_wasm(&executable).is_err());
        fs::write(photon_wasm(&executable), b"wasm").unwrap();
        assert!(require_photon_wasm(&executable).is_ok());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn packaged_version_matches_package_json_and_oauth_stays_static() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../../../packages/pi-agent/package.json")).unwrap();
        assert_eq!(manifest["version"], pi_runtime_version());
        let main = include_str!("../../../packages/pi-agent/src/main.ts");
        let auth = include_str!("../../../packages/pi-agent/src/auth-main.ts");
        assert!(main.contains("registerBunOAuthFlows()"));
        assert!(auth.contains("registerBunOAuthFlows()"));
        let fetch = include_str!("../../../scripts/fetch-bun.sh");
        assert!(fetch.contains(&format!("BUN_VERSION={BUN_VERSION}")));
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

    /// 镜像下的必须是同一个包，不然写死的校验和对不上。
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn bun_mirror_serves_the_same_release_file() {
        // 从右边取两段：文件名，再是版本目录。闭包做不到「返回值借用参数」
        // （两个生命周期对不上），函数签名可以。
        fn tail(url: &str) -> Vec<&str> {
            url.rsplitn(3, '/').take(2).collect()
        }
        let (url, _) = BUN_DOWNLOAD;
        let mirror = bun_mirror_url();
        assert_eq!(tail(&mirror), tail(url), "版本目录和文件名都要一致");
    }
}
