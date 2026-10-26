//! smeltd 自己的命令行：`install`、`uninstall`、`status`、`pair`、`devices`、`mcp`、
//! `--version`。
//!
//! 无头机器上没有桌面来拉起守护，运行时要能自己装成常驻服务（设计文档 D7）。Linux 用
//! systemd 用户单元：跟着这个用户的登录会话走，`loginctl enable-linger` 之后退出登录
//! 也继续跑。没有 systemd 的机器（多半是容器）只装映像，由安装脚本放的 `smelt-up` 拉起
//! 并看着。macOS 上守护照旧由桌面拉起，这里只提示。

#[cfg(any(target_os = "linux", test))]
use std::path::Path;
#[cfg(target_os = "linux")]
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::process::Command;

#[cfg(target_os = "linux")]
const UNIT_NAME: &str = "smeltd.service";

/// 认得的子命令跑完返回退出码；不是子命令（无参数）返回 `None`，照常起守护。
pub(crate) fn maybe_run(args: &[String]) -> Option<i32> {
    let command = args.get(1)?;
    Some(match command.as_str() {
        "--version" | "-V" => {
            println!("smeltd {}", env!("CARGO_PKG_VERSION"));
            0
        }
        "status" => status(),
        "install" => report(install()),
        "uninstall" => report(uninstall()),
        "pair" => report(pair()),
        "devices" => report(devices(&args[2..])),
        "relay" => report(relay(&args[2..])),
        "mcp" => report(mcp(&args[2..])),
        "help" | "--help" | "-h" => {
            print_help();
            0
        }
        other => {
            eprintln!("smeltd：未知命令 {other}");
            print_help();
            2
        }
    })
}

fn print_help() {
    println!(
        "用法：
  smeltd             以前台方式运行守护（通常由 systemd 或桌面拉起）
  smeltd install     装成 systemd 用户服务并立即启动（Linux）；没有 systemd 时只装映像
  smeltd uninstall   停止并移除 systemd 用户服务（Linux）
  smeltd status      查看本机运行时是否在跑
  smeltd pair        开启远程访问，打印一次性配对串和二维码
  smeltd devices     列出已配对设备
  smeltd devices revoke <endpoint-id>
                     撤销一台设备，立即断开它
  smeltd relay [<地址>|off]
                     远程访问用哪个 iroh relay：不带参数看现在的；填自建 relay 的域名、
                     IP 或 URL 就只用它；off 改回 n0 的公共 relay
  smeltd mcp add <id>
                     把内置的 MCP 服务器加进 Pi 的 mcp.json，已经有了就不动
  smeltd --version   打印版本"
    );
}

fn report(result: Result<(), String>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("smeltd：{error}");
            1
        }
    }
}

fn status() -> i32 {
    match smelt_runtime_client::local::local_runtime().describe() {
        Ok(described) => {
            let daemon = described.daemon;
            println!("运行中");
            println!("  版本：{}", daemon.version);
            println!("  pid：{}", daemon.pid);
            println!("  会话：{}", daemon.session_count);
            if let Some(id) = daemon.runtime_id {
                println!("  运行时 id：{id}");
            }
            if let (Some(os), Some(arch)) = (daemon.os, daemon.arch) {
                println!("  平台：{os}/{arch}");
            }
            println!(
                "  socket：{}",
                smelt_runtime_client::local::smeltd_sock_path().display()
            );
            0
        }
        Err(error) => {
            println!("没有在跑（{error}）");
            3
        }
    }
}

fn local() -> smelt_runtime_client::RuntimeClient {
    smelt_runtime_client::local::local_runtime()
}

fn call<C: smelt_runtime_client::protocol::control::ControlCall>(
    params: &C::Params,
) -> Result<C::Result, String> {
    call_with_timeout::<C>(params, smelt_runtime_client::DEFAULT_REQUEST_TIMEOUT)
}

fn call_with_timeout<C: smelt_runtime_client::protocol::control::ControlCall>(
    params: &C::Params,
    timeout: std::time::Duration,
) -> Result<C::Result, String> {
    local()
        .call::<C>(params, timeout)
        .map_err(|error| format!("调用本机运行时失败：{error}"))
}

/// 远程访问没开就打开（保留已配的自建 relay），等它真正跑起来。
fn ensure_remote_running() -> Result<(), String> {
    use smelt_core::control_api::{RemoteConfigGet, RemoteConfigSet, RemoteStatus, RemoteStatusWait};
    use smelt_core::remote_config::{
        REMOTE_STATUS_WAIT_MAX, RemoteAccessState, RemoteStatusWaitParams,
    };
    use smelt_runtime_client::protocol::control::EmptyParams;

    let mut state = call::<RemoteStatus>(&EmptyParams::default())?;
    if matches!(
        state,
        RemoteAccessState::Off | RemoteAccessState::Failed { .. }
    ) {
        let mut config = call::<RemoteConfigGet>(&EmptyParams::default())?;
        if !config.enabled {
            config.enabled = true;
            println!("正在开启远程访问…");
            state = call::<RemoteConfigSet>(&config)?;
        }
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        match &state {
            RemoteAccessState::Running { .. } => return Ok(()),
            RemoteAccessState::Failed { error } if std::time::Instant::now() >= deadline => {
                return Err(format!("远程访问起不来：{error}"));
            }
            _ if std::time::Instant::now() >= deadline => {
                return Err("等远程访问启动超时（60s），稍后再试".to_string());
            }
            _ => {}
        }
        // 运行时在状态变了时才回；一直没变就最多挂 REMOTE_STATUS_WAIT_MAX，回来再看截止。
        state = call_with_timeout::<RemoteStatusWait>(
            &RemoteStatusWaitParams { known: state },
            REMOTE_STATUS_WAIT_MAX + std::time::Duration::from_secs(5),
        )?;
    }
}

fn pair() -> Result<(), String> {
    use smelt_core::control_api::DevicePairBegin;
    use smelt_runtime_client::protocol::control::EmptyParams;

    ensure_remote_running()?;
    let offer = call::<DevicePairBegin>(&EmptyParams::default())?;
    println!();
    println!("在 Smelt 桌面「添加运行时」里粘贴下面的配对串，或用手机扫码：");
    println!();
    println!("{}", offer.pairing_uri);
    println!();
    if let Ok(code) = qrcode::QrCode::new(offer.pairing_uri.as_bytes()) {
        use qrcode::render::unicode::Dense1x2;
        // 终端多是深色底，反色后扫码更稳。
        let image = code
            .render::<Dense1x2>()
            .dark_color(Dense1x2::Light)
            .light_color(Dense1x2::Dark)
            .quiet_zone(true)
            .build();
        println!("{image}");
    }
    println!("配对串只能用一次，10 分钟内有效。");
    println!("配对一台设备，就是把这台机器上你这个用户的 shell 权限交给它。");
    Ok(())
}

fn devices(args: &[String]) -> Result<(), String> {
    use smelt_core::control_api::{DeviceList, DeviceRevoke};
    use smelt_core::remote_config::DeviceIdParams;
    use smelt_runtime_client::protocol::control::EmptyParams;

    match args {
        [] => {
            let list = call::<DeviceList>(&EmptyParams::default())?;
            if list.devices.is_empty() {
                println!("还没有配对过设备。用 smeltd pair 配对一台。");
                return Ok(());
            }
            for device in list.devices {
                let seen = if device.connected {
                    "在线".to_string()
                } else {
                    device
                        .last_seen_at_ms
                        .and_then(chrono::DateTime::from_timestamp_millis)
                        .map(|at| {
                            format!(
                                "最近连接 {}",
                                at.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M")
                            )
                        })
                        .unwrap_or_else(|| "还没连过".to_string())
                };
                println!("{}  {}  {}", device.endpoint_id, device.name, seen);
            }
            Ok(())
        }
        [command, endpoint_id] if command == "revoke" => {
            let result = call::<DeviceRevoke>(&DeviceIdParams {
                endpoint_id: endpoint_id.clone(),
            })?;
            if result.revoked {
                println!("已撤销 {endpoint_id}");
                Ok(())
            } else {
                Err(format!("没有这台设备：{endpoint_id}"))
            }
        }
        _ => Err("用法：smeltd devices [revoke <endpoint-id>]".to_string()),
    }
}

/// 远程访问用哪个 iroh relay。配对串会带上它，所以要在配对之前设好；已经配对的设备要在
/// 桌面上把这台机器的中继改成同一个（机器菜单的「中继…」），不然连不上。
fn relay(args: &[String]) -> Result<(), String> {
    use smelt_core::control_api::{RemoteConfigGet, RemoteConfigSet};
    use smelt_runtime_client::protocol::control::EmptyParams;

    let mut config = call::<RemoteConfigGet>(&EmptyParams::default())?;
    let relay = match args {
        [] => {
            match config.iroh_relay.trim() {
                "" => println!("用 n0 的公共 relay"),
                relay => println!("自建 relay：{relay}"),
            }
            return Ok(());
        }
        [value] if value == "off" => String::new(),
        [value] => {
            smelt_iroh::RelaySettings::parse(value).map_err(|error| format!("{error:#}"))?;
            value.trim().to_string()
        }
        _ => return Err("用法：smeltd relay [<地址>|off]".to_string()),
    };
    if config.iroh_relay == relay {
        println!("没有变化");
        return Ok(());
    }
    config.iroh_relay = relay;
    call::<RemoteConfigSet>(&config)?;
    match config.iroh_relay.as_str() {
        "" => println!("已改用 n0 的公共 relay"),
        relay => println!("已改用自建 relay：{relay}"),
    }
    println!("之后生成的配对串会带上它；已经配对的设备要在桌面上把这台机器的中继改成同一个。");
    Ok(())
}

/// 把内置的 MCP 服务器加进 Pi 的 `mcp.json`。安装脚本会重复跑，所以已经有同名服务器就
/// 算成功、不去动它（可能是用户改过的）。
fn mcp(args: &[String]) -> Result<(), String> {
    use smelt_core::control_api::{PiConfigGet, PiMcpAddPreset};
    use smelt_core::pi_service::McpPresetParams;
    use smelt_runtime_client::protocol::control::EmptyParams;

    match args {
        [command, id] if command == "add" => {
            let servers = call::<PiConfigGet>(&EmptyParams::default())?
                .mcp_servers
                .map_err(|error| format!("读不了 Pi 的 mcp.json：{error}"))?;
            if servers.iter().any(|server| server.name == *id) {
                println!("Pi 的 mcp.json 里已经有 {id}");
                return Ok(());
            }
            call::<PiMcpAddPreset>(&McpPresetParams { id: id.clone() })?;
            println!("已把 {id} 加进 Pi 的 mcp.json");
            Ok(())
        }
        _ => Err("用法：smeltd mcp add <内置服务器 id>".to_string()),
    }
}

/// 这台机器是不是由 systemd 启动的，判断方法同 sd_booted(3)。容器里一般不是：装了
/// systemctl 也连不上 systemd。
#[cfg(target_os = "linux")]
fn systemd_booted() -> bool {
    Path::new("/run/systemd/system").is_dir()
}

#[cfg(target_os = "linux")]
fn unit_path() -> Result<PathBuf, String> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
        .ok_or_else(|| "找不到用户配置目录".to_string())?;
    Ok(config.join("systemd/user").join(UNIT_NAME))
}

#[cfg(any(target_os = "linux", test))]
/// 单元文件内容。`ExecStart` 是启动名 `~/.smelt/bin/smeltd` 本身（指向当前不可变映像
/// 的软链）。升级时先把启动名拨到新映像，优雅停机后在本进程里 exec 那份映像，systemd
/// 看到的还是同一个主进程。`Restart=on-failure` 不会在干净退出后续上，所以升级不能只
/// exit。`KillMode=process`：停服务只停守护本身。
fn unit_contents(exe: &Path) -> String {
    format!(
        "[Unit]
Description=Smelt 运行时（smeltd）
After=network-online.target

[Service]
Type=simple
ExecStart={exe}
NotifyAccess=main
Restart=on-failure
RestartSec=2
KillMode=process

[Install]
WantedBy=default.target
",
        exe = exe.display()
    )
}

#[cfg(target_os = "linux")]
fn systemctl(args: &[&str]) -> Result<(), String> {
    let output = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map_err(|error| format!("无法执行 systemctl：{error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "systemctl --user {} 失败：{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// 把一份 smeltd 装进 `~/.smelt/bin` 之后的情形。
#[cfg(target_os = "linux")]
pub(crate) enum ImageInstall {
    /// 启动名本来就指向这份映像。
    AlreadyInstalled,
    /// 运行时没在跑，启动名已经拨过去，下次拉起就是它。
    Launched,
    /// 正在跑的运行时已经退出，新映像由接替进程拉起。进行中的回合会中断。
    Upgraded,
}

/// 把 `exe` 装进 `~/.smelt/bin`，用和桌面同一套受管布局：发布成不可变映像
/// `smeltd-<指纹>`，启动名 `smeltd` 是指向它的软链。运行时已经在跑就停掉旧进程并
/// 拉起这份映像，不等待 agent 空闲。`smeltd install` 和桌面请运行时升级
/// （[`crate::runtime_upgrade`]）共用这一份。
#[cfg(target_os = "linux")]
pub(crate) fn install_image(exe: &Path) -> Result<ImageInstall, String> {
    use smelt_core::daemon_image;

    let dir = bin_dir()?;
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("创建 {} 失败：{error}", dir.display()))?;
    let launch = dir.join(daemon_image::LAUNCH_NAME);
    let image = daemon_image::publish_image(&dir, exe)
        .map_err(|error| format!("发布 smeltd 映像失败：{error}"))?;
    // 包里带着 smelt-pi 就一起发布、拨链接，和守护换不换代无关。从 `~/.smelt/bin` 或
    // 开发目录 install 时旁边没有，交给运行时启动时按自己的版本补（`ensure_smelt_pi`）。
    let package = exe.parent().ok_or("smeltd 没有所在目录")?;
    if smelt_core::pi_image::bundled(package) {
        smelt_core::pi_image::publish(&dir, package)
            .map_err(|error| format!("发布 smelt-pi 失败：{error}"))?;
    }
    let installed = std::fs::canonicalize(&launch).ok() == std::fs::canonicalize(&image).ok();
    let running = smelt_runtime_client::local::local_runtime()
        .describe()
        .is_ok();
    let outcome = if installed {
        ImageInstall::AlreadyInstalled
    } else if running {
        daemon_image::retarget_launch_after_commit(&image)
            .map_err(|error| format!("设置启动名失败：{error}"))?;
        match crate::request_self_upgrade(&image) {
            crate::SelfUpgradeOutcome::Upgraded => ImageInstall::Upgraded,
            crate::SelfUpgradeOutcome::Failed(error) => {
                return Err(format!("运行时换到新版本失败：{error}"));
            }
        }
    } else {
        daemon_image::retarget_launch_after_commit(&image)
            .map_err(|error| format!("设置启动名失败：{error}"))?;
        ImageInstall::Launched
    };
    if let Err(error) = daemon_image::reap_unused_images(&dir) {
        eprintln!("回收旧映像失败：{error}");
    }
    if let Err(error) = smelt_core::pi_image::reap_unused(&dir) {
        eprintln!("回收旧 smelt-pi 失败：{error}");
    }
    Ok(outcome)
}

/// `~/.smelt/bin`：启动名、映像和随运行时一起装的小工具都在这里。
#[cfg(target_os = "linux")]
pub(crate) fn bin_dir() -> Result<PathBuf, String> {
    Ok(smelt_paths::smelt_home()
        .ok_or_else(|| "找不到 $HOME".to_string())?
        .join("bin"))
}

/// 把正在执行的这份 smeltd 装好，返回启动名。
#[cfg(target_os = "linux")]
fn install_current_exe() -> Result<PathBuf, String> {
    let exe =
        std::env::current_exe().map_err(|error| format!("找不到 smeltd 自身路径：{error}"))?;
    match install_image(&exe)? {
        ImageInstall::AlreadyInstalled => println!("这个版本已经装好了"),
        ImageInstall::Upgraded => {
            println!("运行时已重启到新版本。进行中的回合会中断，历史记录还在")
        }
        ImageInstall::Launched => {}
    }
    Ok(bin_dir()?.join(smelt_core::daemon_image::LAUNCH_NAME))
}

#[cfg(target_os = "linux")]
fn install() -> Result<(), String> {
    if !systemd_booted() {
        // 没有服务管理器可登记。已经在跑（smelt-up 拉起的）就照常交接到新映像。
        install_current_exe()?;
        println!("这台机器没有 systemd，没装服务：运行时由 smelt-up 拉起并看着");
        return Ok(());
    }
    // 已经在跑、却不归 systemd 管（手动起的）：收编不进来，装上去也只是两个实例抢锁。
    if smelt_runtime_client::local::local_runtime()
        .describe()
        .is_ok()
        && !unit_active()
    {
        return Err(
            "运行时正在 systemd 之外运行（smeltd status 看 pid）。先停掉它再装；它的会话会随之结束"
                .to_string(),
        );
    }
    let exe = install_current_exe()?;
    let unit = unit_path()?;
    if let Some(dir) = unit.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|error| format!("创建 {} 失败：{error}", dir.display()))?;
    }
    std::fs::write(&unit, unit_contents(&exe))
        .map_err(|error| format!("写入 {} 失败：{error}", unit.display()))?;
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", "--now", UNIT_NAME])?;
    println!("已安装并启动：{}", unit.display());
    if !linger_enabled() {
        println!(
            "提示：退出登录后 systemd 会停掉用户服务。要让运行时一直在线，执行：\n  loginctl enable-linger {}",
            whoami()
        );
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn uninstall() -> Result<(), String> {
    if !systemd_booted() {
        return Err(
            "这台机器没有 systemd，没有服务可移除；运行时由 smelt-up 看着，停掉它即可".to_string(),
        );
    }
    let unit = unit_path()?;
    // 服务可能已经停了或从没装过，停和禁用失败都不算错。
    let _ = systemctl(&["disable", "--now", UNIT_NAME]);
    match std::fs::remove_file(&unit) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("删除 {} 失败：{error}", unit.display())),
    }
    systemctl(&["daemon-reload"])?;
    println!("已移除 {UNIT_NAME}");
    Ok(())
}

#[cfg(target_os = "linux")]
fn unit_active() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", UNIT_NAME])
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(target_os = "linux")]
fn whoami() -> String {
    std::env::var("USER").unwrap_or_else(|_| "$USER".to_string())
}

#[cfg(target_os = "linux")]
fn linger_enabled() -> bool {
    Path::new("/var/lib/systemd/linger").join(whoami()).exists()
}

#[cfg(not(target_os = "linux"))]
fn install() -> Result<(), String> {
    Err("只有 Linux 上要手动安装服务；macOS 上由 Smelt 桌面拉起守护".to_string())
}

#[cfg(not(target_os = "linux"))]
fn uninstall() -> Result<(), String> {
    Err("只有 Linux 上才有 systemd 服务可移除".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_runs_the_resolved_binary_and_leaves_sessions_to_the_daemon() {
        let unit = unit_contents(Path::new("/home/me/.smelt/bin/smeltd"));
        assert!(unit.contains("ExecStart=/home/me/.smelt/bin/smeltd\n"));
        assert!(unit.contains("KillMode=process"));
        assert!(unit.contains("NotifyAccess=main"));
        assert!(unit.contains("WantedBy=default.target"));
    }

    #[test]
    fn no_arguments_means_run_the_daemon() {
        assert_eq!(maybe_run(&["smeltd".to_string()]), None);
        assert_eq!(
            maybe_run(&["smeltd".to_string(), "--version".to_string()]),
            Some(0)
        );
    }
}
