//! 随应用发布的 `smelt-pi` 和 Bun。
//!
//! Pi 是 `bun build --compile` 出来的单个可执行文件，和 `smeltd` 放在同一目录
//! （macOS 的 `Contents/MacOS`，Linux 的 `~/.smelt/bin`）。不再把源码嵌进 Rust
//! 二进制，也不再在用户机器上 `bun install`。
//!
//! Bun 只留给 MCP 预设的 `bun x` 和 Claude/Codex 的 `bunx`。它同样打进应用，
//! 不再首启下载。`~/.smelt/runtime/bin/bun` 是指向这份 Bun 的符号链接，
//! `mcp.json` 里写的还是这条路径。

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 与 `scripts/fetch-bun.sh` 钉死的版本一致。运行时不再按这个版本下载。
#[cfg(test)]
const BUN_VERSION: &str = "1.4.0";

pub use smelt_model::agent_paths::MANAGED_BUN_LAUNCHER_HOME_PATH;

include!(concat!(env!("OUT_DIR"), "/pi_agent_version.rs"));

/// `~/.smelt/runtime/bin/bun`，相对 runtime 根目录。
const BUN_LAUNCHER: &str = "bin/bun";

#[derive(Clone, Debug)]
pub struct ManagedBunRuntime {
    pub path: PathBuf,
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
        command
    }
}

pub(crate) fn pi_runtime_version() -> &'static str {
    PI_AGENT_RUNTIME_VERSION
}

/// 定位 `smelt-pi`，并在 Bun 也在旁边时把 `~/.smelt/runtime/bin/bun` 指过去。
/// Pi 会话不依赖 Bun；MCP 预设写进 `mcp.json` 的是那条固定路径，所以能指就指。
pub(crate) fn sync_managed_pi_agent(_status: &dyn Fn(&str)) -> Result<ManagedPiRuntime, String> {
    let executable = locate_smelt_pi()?;
    require_photon_wasm(&executable)?;
    if let Ok(bun) = locate_bundled_bun() {
        publish_managed_bun(&bun)?;
    }
    Ok(ManagedPiRuntime { executable })
}

pub fn sync_managed_bun(_status: &dyn Fn(&str)) -> Result<ManagedBunRuntime, String> {
    let path = locate_bundled_bun()?;
    publish_managed_bun(&path)?;
    Ok(ManagedBunRuntime { path })
}

/// 诊断用：不下载、不写启动器。二进制在且旁边有 photon wasm 才算就绪。
pub(crate) fn managed_pi_diagnostic() -> Result<(PathBuf, bool), String> {
    let executable = locate_smelt_pi()?;
    Ok((executable.clone(), photon_wasm(&executable).is_file()))
}

fn publish_managed_bun(bun: &Path) -> Result<(), String> {
    let Some(home) = smelt_paths::smelt_home() else {
        return Err("找不到 $HOME".to_string());
    };
    let launcher = publish_bun_launcher(&home.join("runtime"), bun)?;
    // 测试二进制的 ~/.pi 不是沙箱。npmCommand 的写入由单测直接打到临时目录。
    if smelt_paths::running_under_test() {
        return Ok(());
    }
    ensure_pi_npm_command(&crate::pi_model_settings::pi_agent_dir(), &launcher)
}

fn locate_smelt_pi() -> Result<PathBuf, String> {
    first_executable([
        env_path("SMELT_PI"),
        sibling("smelt-pi"),
        dev_artifact("smelt-pi"),
    ])
    .ok_or_else(|| {
        "找不到 smelt-pi。它应和 smeltd 在同一目录（应用内的 Contents/MacOS，或 Linux 的 ~/.smelt/bin）。开发时先运行 scripts/build-smelt-pi.sh，或设置 SMELT_PI。"
            .to_string()
    })
}

fn locate_bundled_bun() -> Result<PathBuf, String> {
    first_executable([
        env_path("SMELT_BUN"),
        sibling("bun"),
        dev_bun(),
    ])
    .ok_or_else(|| {
        "找不到随应用发布的 Bun。它应和 smeltd 在同一目录。开发时先运行 scripts/fetch-bun.sh，或设置 SMELT_BUN。"
            .to_string()
    })
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

fn sibling(name: &str) -> Option<PathBuf> {
    std::env::current_exe().ok().map(|exe| exe.with_file_name(name))
}

fn dev_artifact(name: &str) -> Option<PathBuf> {
    target_dir().map(|target| target.join("smelt-pi").join(name))
}

fn dev_bun() -> Option<PathBuf> {
    target_dir().map(|target| target.join("bundled-bun").join(host_bun_id()).join("bun"))
}

fn target_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors()
        .find(|path| path.file_name().is_some_and(|name| name == "target"))
        .map(Path::to_path_buf)
}

fn host_bun_id() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "darwin-arm64",
        ("macos", "x86_64") => "darwin-x64",
        ("linux", "x86_64") => "linux-x64",
        ("linux", "aarch64") => "linux-arm64",
        _ => "unsupported",
    }
}

fn first_executable(candidates: impl IntoIterator<Item = Option<PathBuf>>) -> Option<PathBuf> {
    candidates
        .into_iter()
        .flatten()
        .find(|path| executable_file(path))
}

fn executable_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| {
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
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

/// 把 `<runtime>/bin/bun` 原子地指向 `bun`。正在通过旧链接启动的进程不会撞上「文件不存在」。
pub(crate) fn publish_bun_launcher(runtime_root: &Path, bun: &Path) -> Result<PathBuf, String> {
    let launcher = runtime_root.join(BUN_LAUNCHER);
    let bun = fs::canonicalize(bun).unwrap_or_else(|_| bun.to_path_buf());
    if fs::read_link(&launcher).is_ok_and(|target| target == bun) {
        return Ok(launcher);
    }
    let dir = launcher.parent().unwrap_or(runtime_root);
    fs::create_dir_all(dir).map_err(|error| format!("创建 {} 失败：{error}", dir.display()))?;
    let temporary = dir.join(format!(".bun-{}-{}", std::process::id(), uuid::Uuid::new_v4()));
    let result = std::os::unix::fs::symlink(&bun, &temporary)
        .and_then(|()| fs::rename(&temporary, &launcher))
        .map_err(|error| format!("更新 Bun 固定入口 {} 失败：{error}", launcher.display()));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    Ok(launcher)
}

/// Pi 安装扩展依赖时，只有命令的 basename 是 `bun` 才会改用 `--cwd` 和 `--omit=peer`。
pub(crate) fn ensure_pi_npm_command(agent_dir: &Path, launcher: &Path) -> Result<(), String> {
    let path = agent_dir.join("settings.json");
    let mut value = crate::pi_model_settings::read_or_create_json(&path)?;
    let command = serde_json::json!([launcher.to_string_lossy()]);
    if value.get("npmCommand") == Some(&command) {
        return Ok(());
    }
    let object = value
        .as_object_mut()
        .ok_or_else(|| format!("{} 的顶层不是对象", path.display()))?;
    object.insert("npmCommand".to_string(), command);
    crate::pi_model_settings::write_json_file(&path, &value, false)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }

    #[test]
    fn bun_launcher_follows_the_bundled_binary() {
        let runtime =
            std::env::temp_dir().join(format!("smelt-bun-launcher-{}", uuid::Uuid::new_v4()));
        let bun = runtime.join("Smelt.app/Contents/MacOS/bun");
        fs::create_dir_all(bun.parent().unwrap()).unwrap();
        fs::write(&bun, b"bun").unwrap();

        let first = publish_bun_launcher(&runtime, &bun).unwrap();
        assert_eq!(fs::read_link(&first).unwrap(), fs::canonicalize(&bun).unwrap());
        publish_bun_launcher(&runtime, &bun).unwrap();
        assert!(
            MANAGED_BUN_LAUNCHER_HOME_PATH.ends_with(BUN_LAUNCHER),
            "写进外部配置的路径必须就是这个入口"
        );
        let leftovers = fs::read_dir(runtime.join("bin")).unwrap().count();
        assert_eq!(leftovers, 1, "临时链接不能留下");
        fs::remove_dir_all(runtime).ok();
    }

    #[test]
    fn npm_command_points_at_the_bun_launcher_and_keeps_other_settings() {
        let dir = std::env::temp_dir().join(format!("smelt-pi-npm-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("settings.json"),
            r#"{"defaultProvider":"openai","defaultModel":"gpt"}"#,
        )
        .unwrap();
        let launcher = dir.join("runtime/bin/bun");
        ensure_pi_npm_command(&dir, &launcher).unwrap();
        ensure_pi_npm_command(&dir, &launcher).unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.join("settings.json")).unwrap()).unwrap();
        assert_eq!(value["defaultProvider"], "openai");
        assert_eq!(value["npmCommand"], serde_json::json!([launcher.to_string_lossy()]));
        fs::remove_dir_all(dir).ok();
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
        let manifest: serde_json::Value = serde_json::from_str(include_str!(
            "../../../packages/pi-agent/package.json"
        ))
        .unwrap();
        assert_eq!(manifest["version"], pi_runtime_version());
        let main = include_str!("../../../packages/pi-agent/src/main.ts");
        let auth = include_str!("../../../packages/pi-agent/src/auth-main.ts");
        assert!(main.contains("registerBunOAuthFlows()"));
        assert!(auth.contains("registerBunOAuthFlows()"));
        let fetch = include_str!("../../../scripts/fetch-bun.sh");
        assert!(fetch.contains(&format!("BUN_VERSION={BUN_VERSION}")));
    }
}
