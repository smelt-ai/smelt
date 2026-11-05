//! Pi 跑的是 `~/.smelt/bin/smelt-agent` 指向的那份编译好的 smelt-agent（见 `agent_image`）。
//! 需要 Bun 的地方（MCP 预设的 `bun x`）也用它：smelt-agent 带上 `BUN_BE_BUN=1` 就是 Bun 命令行，
//! 不再另外下载 Bun。

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

include!(concat!(env!("OUT_DIR"), "/pi_agent_version.rs"));

/// 起 smelt-agent 时用这个环境变量告诉它守护这边的约定版本（`PI_SMELT_PROTOCOL`）。
/// smelt-agent 和守护分开发布，对不上它会拒绝启动，并在 stderr 说明。
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

    /// 起 smelt-agent 的子命令（登录、MCP 登录）。和 Pi 会话一样铺 login shell 环境：
    /// 守护拿不到用户 rc 里的代理变量，子命令要联网，缺了它在只放行代理的网络里会挂住。
    pub(crate) fn command(&self, subcommand: Option<&str>) -> Command {
        let mut command = Command::new(&self.executable);
        if let Some(subcommand) = subcommand {
            command.arg(subcommand);
        }
        let protocol = pi_protocol();
        crate::login_env::apply_login_environment(
            &mut command,
            [(PI_PROTOCOL_ENV, protocol.as_str())],
        );
        command
    }
}

pub(crate) fn pi_runtime_version() -> &'static str {
    PI_AGENT_RUNTIME_VERSION
}

/// 定位编译好的 `smelt-agent`。不下载、不写 `settings.json`。
pub(crate) fn sync_managed_pi_agent(_status: &dyn Fn(&str)) -> Result<ManagedPiRuntime, String> {
    let executable = locate_smelt_agent()?;
    require_photon_wasm(&executable)?;
    Ok(ManagedPiRuntime { executable })
}

/// 当前这一套 smelt-agent 里的内置技能目录。还没装 smelt-agent 时没有。
pub(crate) fn builtin_skills_dir() -> Option<PathBuf> {
    let executable = locate_smelt_agent().ok()?;
    Some(executable.parent()?.join(crate::agent_image::SKILLS_DIR))
}

/// 诊断用：不下载、不写启动器。二进制在且旁边有 photon wasm 才算就绪。
pub(crate) fn managed_pi_diagnostic() -> Result<(PathBuf, bool), String> {
    let executable = locate_smelt_agent()?;
    Ok((executable.clone(), photon_wasm(&executable).is_file()))
}

pub(crate) fn locate_smelt_agent() -> Result<PathBuf, String> {
    let home = smelt_paths::smelt_home().ok_or("找不到 home 目录")?;
    locate_smelt_agent_in(&home.join("bin"))
}

/// 守护启动后在后台调一次，清掉改名前 smelt-pi 留下的链接和目录（见
/// `agent_image::reap_legacy`）。只能由守护调：它在跑说明旧守护已经交接走了。
pub fn reap_legacy_smelt_pi() {
    let Some(home) = smelt_paths::smelt_home() else {
        return;
    };
    if let Err(error) = crate::agent_image::reap_legacy(&home.join("bin")) {
        crate::app_log::error("smeltd", &format!("清理旧 smelt-pi 失败：{error}"));
    }
}

fn locate_smelt_agent_in(bin: &Path) -> Result<PathBuf, String> {
    crate::agent_image::current(bin).ok_or_else(|| {
        format!(
            "找不到 smelt-agent（{}）。桌面重新打开 Smelt 会从 App 里装上；Linux 上运行时启动时会按自己的版本补上，或重新运行安装脚本。",
            bin.join(crate::agent_image::LAUNCH_NAME).display()
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
            "smelt-agent 旁边缺少 photon_rs_bg.wasm（{}）",
            wasm.display()
        ))
    }
}

/// 守护启动后在后台调一次，清掉单独下载 Bun 那一套留下的东西：`mcp.json` 里指着它的条目、
/// 整个 `runtime` 目录、旧版 `bunx` 下载的适配器缓存。`runtime` 里只剩遗留物：Bun 和 Pi
/// 源码的 generation，已删除的 DeepSeek Harness（`dsh-npx`）和旧插件体系（`shell-plugins`）
/// 留下的目录。Windows 上还有交接前的 MCP 进程占着旧 `bun.exe` 时删不干净，留到下次启动。
///
/// 整个删不会拽掉还在跑的进程：升级不交接子进程，旧守护在交出回执前已经收掉所有 ACP 进程组
/// 和终端，新守护起来时没有谁还在用旧 Bun；漏网的只有上面说的 Windows 那种，下次再删。
pub fn retire_managed_bun() {
    crate::pi_mcp::retire_managed_bun_entries();
    if let Some(home) = smelt_paths::smelt_home() {
        let runtime = home.join("runtime");
        match fs::remove_dir_all(&runtime) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => crate::app_log::error(
                "smeltd",
                &format!("清理旧运行时目录 {} 失败：{error}", runtime.display()),
            ),
        }
    }
    if let Some(cache_root) = bun_cache_dir() {
        cleanup_retired_adapter_cache(&cache_root);
    }
}

/// 旧版 Smelt 用 `bunx` 下载适配器时的 Bun 包缓存。登录 shell 里显式配置的绝对路径优先；
/// 相对路径会随会话 cwd 漂移，无法安全判断删除目标，因此不做自动清理。
fn bun_cache_dir() -> Option<std::path::PathBuf> {
    let home = dirs::home_dir()?;
    let configured = crate::login_env::login_environment()
        .get("BUN_INSTALL_CACHE_DIR")
        .filter(|value| !value.trim().is_empty())
        .map(|value| crate::workspace_override::expand_tilde(value));
    let path = configured
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| home.join(".bun/install/cache"));
    // 即便用户把缓存变量误设成根目录或 home，也不能让自动回收触碰这种宽目标。
    (path.is_absolute() && path.parent().is_some() && path != home).then_some(path)
}

fn safe_bun_cache_component(value: &str) -> bool {
    !value.is_empty() && value != "." && value != ".." && !value.contains(['/', '\\'])
}

/// 返回 Bun 缓存中一个 npm 包的父目录与无 scope 包名。
fn bun_cache_package_location(
    cache_root: &std::path::Path,
    package: &str,
) -> Option<(std::path::PathBuf, String)> {
    if let Some(scoped) = package.strip_prefix('@') {
        let (scope, name) = scoped.split_once('/')?;
        if !safe_bun_cache_component(scope) || !safe_bun_cache_component(name) {
            return None;
        }
        return Some((cache_root.join(format!("@{scope}")), name.to_string()));
    }
    safe_bun_cache_component(package).then(|| (cache_root.to_path_buf(), package.to_string()))
}

fn bun_cache_entry_matches(name: &str, stem: &str) -> bool {
    name == stem
        || name
            .strip_prefix(stem)
            .is_some_and(|rest| rest.starts_with("@@@"))
}

fn remove_bun_cache_entry(path: &std::path::Path) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if metadata.file_type().is_symlink() || metadata.is_file() {
        std::fs::remove_file(path).is_ok()
    } else if metadata.is_dir() {
        std::fs::remove_dir_all(path).is_ok()
    } else {
        false
    }
}

fn cleanup_adapter_release(cache_root: &std::path::Path, package: &str, version: &str) -> usize {
    if !safe_bun_cache_component(version) {
        return 0;
    }
    let Some((package_parent, package_name)) = bun_cache_package_location(cache_root, package)
    else {
        return 0;
    };

    let mut removed = 0;
    let actual_stem = format!("{package_name}@{version}");
    if let Ok(entries) = std::fs::read_dir(&package_parent) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name
                .to_str()
                .is_some_and(|name| bun_cache_entry_matches(name, &actual_stem))
                && remove_bun_cache_entry(&entry.path())
            {
                removed += 1;
            }
        }
    }

    // Bun 还会维护 `<scope>/<package>/<version>@@@N` 别名符号链接。只遍历真正
    // 的目录，拒绝跟随异常符号链接，避免缓存损坏时越过 cache_root。
    let aliases = package_parent.join(&package_name);
    if std::fs::symlink_metadata(&aliases).is_ok_and(|metadata| metadata.is_dir()) {
        if let Ok(entries) = std::fs::read_dir(&aliases) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if name
                    .to_str()
                    .is_some_and(|name| bun_cache_entry_matches(name, version))
                    && remove_bun_cache_entry(&entry.path())
                {
                    removed += 1;
                }
            }
        }
        let _ = std::fs::remove_dir(&aliases); // 仅空目录会成功；当前版本别名不会受影响。
    }
    removed
}

/// 清掉旧版出厂默认命令（`bunx --bun 包@版本`）下载过的适配器包，不碰别的版本和共享依赖。
fn cleanup_retired_adapter_cache(cache_root: &std::path::Path) -> usize {
    // 内置适配器的旧名字仍是出厂命令，但运行时不再按字面用 bunx 跑它，缓存也是遗留物。
    let builtin = crate::agent_kind::BUILTIN_ADAPTER_COMMANDS
        .iter()
        .map(|adapter| adapter.legacy);
    crate::agent_kind::ConversationAgentKind::ALL
        .into_iter()
        .flat_map(|agent| agent.descriptor().previous_default_cmds.iter().copied())
        .chain(builtin)
        .filter_map(bunx_package)
        .map(|(package, version)| cleanup_adapter_release(cache_root, package, version))
        .sum()
}

/// `bunx [--bun] 包@版本` 里的包名和版本。
fn bunx_package(command: &str) -> Option<(&str, &str)> {
    let mut words = command.split_whitespace();
    if words.next()? != "bunx" {
        return None;
    }
    let spec = words.find(|word| !word.starts_with("--"))?;
    // 带 scope 的包名本身以 `@` 开头，版本在最后一个 `@` 之后。
    let (package, version) = spec.rsplit_once('@')?;
    (!package.is_empty()).then_some((package, version))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn executable(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, bytes).unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    #[test]
    fn process_args_are_the_binary_then_the_existing_flags() {
        let runtime = ManagedPiRuntime {
            executable: PathBuf::from("/Applications/Smelt.app/Contents/MacOS/smelt-agent"),
        };
        assert_eq!(
            runtime.process_args(["--offline".into(), "--no-approve".into()]),
            vec![
                "/Applications/Smelt.app/Contents/MacOS/smelt-agent",
                "--offline",
                "--no-approve",
            ]
        );
        assert_eq!(
            runtime.command(Some("auth")).get_program(),
            "/Applications/Smelt.app/Contents/MacOS/smelt-agent"
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
            "子命令也要带约定版本，不然 smelt-agent 会拒绝启动"
        );
    }

    /// 守护是 launchd 拉起的，没有用户 rc 里的 `HTTPS_PROXY`。登录助手少了它会
    /// 直连 github.com，在只放行代理的网络里一直挂着；Pi 会话那边早就铺了 login
    /// 环境，子命令得和它一样。
    #[test]
    fn subcommands_run_with_the_login_shell_environment() {
        let runtime = ManagedPiRuntime {
            executable: PathBuf::from("/Applications/Smelt.app/Contents/MacOS/smelt-agent"),
        };
        let command = runtime.command(Some("auth"));
        let env = |name: &str| {
            command
                .get_envs()
                .find(|(key, _)| *key == name)
                .and_then(|(_, value)| value)
                .map(|value| value.to_string_lossy().into_owned())
        };
        assert_eq!(
            env("PATH").as_deref(),
            Some(crate::login_env::login_path())
        );
        for (name, value) in crate::login_env::login_environment() {
            if name == "PATH" || name == PI_PROTOCOL_ENV || name == "NPM_CONFIG_REGISTRY" {
                continue;
            }
            assert_eq!(env(name).as_ref(), Some(value), "{name} 没传给子命令");
        }
    }

    #[cfg(unix)]
    #[test]
    fn smelt_pi_resolves_through_the_launch_link_to_its_real_path() {
        let root = std::env::temp_dir().join(format!(
            "smelt-agent-locate-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let error = locate_smelt_agent_in(&bin).expect_err("没发布过必须失败");
        assert!(error.contains("找不到 smelt-agent"), "实际错误：{error}");

        let published = bin.join(format!("smelt-agent-{}", "ab".repeat(32))).join("smelt-agent");
        executable(&published, b"pi");
        std::os::unix::fs::symlink(
            published.strip_prefix(&bin).unwrap(),
            bin.join("smelt-agent"),
        )
        .unwrap();
        assert_eq!(
            locate_smelt_agent_in(&bin).unwrap(),
            fs::canonicalize(&published).unwrap(),
            "要 exec 解析后的真实路径，链接换了也不影响已经起来的 Pi"
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn photon_wasm_must_sit_beside_the_binary() {
        let dir = std::env::temp_dir().join(format!("smelt-agent-wasm-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let executable = dir.join("smelt-agent");
        fs::write(&executable, b"pi").unwrap();
        assert!(require_photon_wasm(&executable).is_err());
        fs::write(photon_wasm(&executable), b"wasm").unwrap();
        assert!(require_photon_wasm(&executable).is_ok());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn packaged_version_matches_package_json_and_oauth_stays_static() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../../../agent/package.json")).unwrap();
        assert_eq!(manifest["version"], pi_runtime_version());
        let main = include_str!("../../../agent/src/main.ts");
        let auth = include_str!("../../../agent/src/auth-main.ts");
        assert!(main.contains("registerBunOAuthFlows()"));
        assert!(auth.contains("registerBunOAuthFlows()"));
    }

    #[test]
    fn retired_defaults_name_their_adapter_packages() {
        assert_eq!(
            bunx_package("bunx --bun @agentclientprotocol/claude-agent-acp@0.78.0"),
            Some(("@agentclientprotocol/claude-agent-acp", "0.78.0"))
        );
        assert_eq!(
            bunx_package("bunx --bun pi-acp@0.0.33"),
            Some(("pi-acp", "0.0.33"))
        );
        assert_eq!(bunx_package("codex app-server"), None);
    }

    #[cfg(unix)]
    #[test]
    fn retired_adapter_cache_cleanup_is_targeted() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "smelt-adapter-cache-cleanup-{}",
            uuid::Uuid::new_v4()
        ));
        let scope = root.join("@agentclientprotocol");
        let old = scope.join("claude-agent-acp@0.59.0@@@1");
        let retired = scope.join("claude-agent-acp@0.78.0@@@1");
        let current = scope.join("claude-agent-acp@0.100.0@@@1");
        let unknown = scope.join("claude-agent-acp@0.58.0@@@1");
        let shared_sdk = scope.join("sdk@1.2.1@@@1");
        let aliases = scope.join("claude-agent-acp");
        let old_alias = aliases.join("0.59.0@@@1");
        let retired_alias = aliases.join("0.78.0@@@1");
        let current_alias = aliases.join("0.100.0@@@1");
        let retired_pi = root.join("pi-acp@0.0.33@@@1");
        let pi_aliases = root.join("pi-acp");
        let retired_pi_alias = pi_aliases.join("0.0.33@@@1");
        for dir in [&old, &retired, &current, &unknown, &shared_sdk, &aliases] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::create_dir_all(&retired_pi).unwrap();
        std::fs::create_dir_all(&pi_aliases).unwrap();
        symlink(&old, &old_alias).unwrap();
        symlink(&retired, &retired_alias).unwrap();
        symlink(&current, &current_alias).unwrap();
        symlink(&retired_pi, &retired_pi_alias).unwrap();

        let removed = cleanup_retired_adapter_cache(&root);

        assert!(removed >= 6, "旧实体目录和版本别名都应被清掉");
        assert!(!old.exists());
        assert!(std::fs::symlink_metadata(&old_alias).is_err());
        assert!(!retired.exists(), "刚卸任的 0.78.0 缓存应清掉");
        assert!(std::fs::symlink_metadata(&retired_alias).is_err());
        assert!(!retired_pi.exists(), "已退役的 pi-acp 缓存应清掉");
        assert!(std::fs::symlink_metadata(&retired_pi_alias).is_err());
        assert!(current.is_dir(), "没作为 bunx 默认值发布过的版本不删");
        assert!(current_alias.is_symlink(), "它的版本别名也不删");
        assert!(
            unknown.is_dir(),
            "没有作为 Smelt 默认值发布过的版本不应删除"
        );
        assert!(shared_sdk.is_dir(), "共享依赖缓存不属于适配器清理范围");
        let _ = std::fs::remove_dir_all(&root);
    }
}
