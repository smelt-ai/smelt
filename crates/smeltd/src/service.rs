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
            println!("smeltd {}", smelt_version::VERSION);
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
            eprintln!("smeltd: unknown command {other}");
            print_help();
            2
        }
    })
}

fn print_help() {
    println!(
        "Usage:
  smeltd             Run the runtime in the foreground (usually started by systemd or the desktop)
  smeltd install     Install and start it as a systemd user service (Linux); without systemd, only install the image
  smeltd uninstall   Stop and remove the systemd user service (Linux)
  smeltd status      Show whether the local runtime is running
  smeltd pair        Turn on remote access and print a one-time pairing string and QR code
  smeltd devices     List paired devices
  smeltd devices revoke <endpoint-id>
                     Revoke a device and disconnect it right away
  smeltd relay [<address>|off]
                     Which iroh relay remote access uses: no argument shows the current one; a
                     self-hosted relay's domain, IP or URL uses only that; off goes back to n0's public relay
  smeltd mcp add <id>
                     Add a built-in MCP server to Pi's mcp.json; does nothing if it's already there
  smeltd --version   Print the version"
    );
}

fn report(result: Result<(), String>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("smeltd: {error}");
            1
        }
    }
}

fn status() -> i32 {
    match smelt_runtime_client::local::local_runtime().describe() {
        Ok(described) => {
            let daemon = described.daemon;
            println!("Running");
            println!("  version: {}", daemon.version);
            println!("  pid: {}", daemon.pid);
            println!("  sessions: {}", daemon.session_count);
            if let Some(id) = daemon.runtime_id {
                println!("  runtime id: {id}");
            }
            if let (Some(os), Some(arch)) = (daemon.os, daemon.arch) {
                println!("  platform: {os}/{arch}");
            }
            println!(
                "  socket: {}",
                smelt_runtime_client::local::smeltd_sock_path().display()
            );
            0
        }
        Err(error) => {
            println!("Not running ({error})");
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
        .map_err(|error| format!("Couldn't call the local runtime: {error}"))
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
            println!("Turning on remote access…");
            state = call::<RemoteConfigSet>(&config)?;
        }
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        match &state {
            RemoteAccessState::Running { .. } => return Ok(()),
            RemoteAccessState::Failed { error } if std::time::Instant::now() >= deadline => {
                return Err(format!("Remote access couldn't start: {error}"));
            }
            _ if std::time::Instant::now() >= deadline => {
                return Err(
                    "Timed out waiting for remote access to start (60s); try again later"
                        .to_string(),
                );
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
    println!(
        "Paste this pairing string into \"Add runtime\" in Smelt on your desktop, or scan the code with your phone:"
    );
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
    println!("The pairing string works once and expires in 10 minutes.");
    println!("Pairing a device gives it your user's shell access on this machine.");
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
                println!("No paired devices yet. Use smeltd pair to pair one.");
                return Ok(());
            }
            for device in list.devices {
                let seen = if device.connected {
                    "online".to_string()
                } else {
                    device
                        .last_seen_at_ms
                        .and_then(chrono::DateTime::from_timestamp_millis)
                        .map(|at| {
                            format!(
                                "last seen {}",
                                at.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M")
                            )
                        })
                        .unwrap_or_else(|| "never connected".to_string())
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
                println!("Revoked {endpoint_id}");
                Ok(())
            } else {
                Err(format!("No such device: {endpoint_id}"))
            }
        }
        _ => Err("Usage: smeltd devices [revoke <endpoint-id>]".to_string()),
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
                "" => println!("Using n0's public relay"),
                relay => println!("Self-hosted relay: {relay}"),
            }
            return Ok(());
        }
        [value] if value == "off" => String::new(),
        [value] => {
            smelt_iroh::RelaySettings::parse(value).map_err(|error| format!("{error:#}"))?;
            value.trim().to_string()
        }
        _ => return Err("Usage: smeltd relay [<address>|off]".to_string()),
    };
    if config.iroh_relay == relay {
        println!("No change");
        return Ok(());
    }
    config.iroh_relay = relay;
    call::<RemoteConfigSet>(&config)?;
    match config.iroh_relay.as_str() {
        "" => println!("Switched to n0's public relay"),
        relay => println!("Switched to self-hosted relay: {relay}"),
    }
    println!(
        "New pairing strings will include it; for devices already paired, set this machine's relay to the same one on the desktop."
    );
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
                .map_err(|error| format!("Couldn't read Pi's mcp.json: {error}"))?;
            if servers.iter().any(|server| server.name == *id) {
                println!("Pi's mcp.json already has {id}");
                return Ok(());
            }
            call::<PiMcpAddPreset>(&McpPresetParams { id: id.clone() })?;
            println!("Added {id} to Pi's mcp.json");
            Ok(())
        }
        _ => Err("Usage: smeltd mcp add <built-in server id>".to_string()),
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
        .ok_or_else(|| "Couldn't find the user config directory".to_string())?;
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
        .map_err(|error| format!("Couldn't run systemctl: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "systemctl --user {} failed: {}",
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
    /// 启动名已经拨到新映像，正在跑的运行时却没换过去，还在跑旧版本。2026.1028 之前的
    /// 守护按 fd 交接换代，新映像不再接交接，它等不到 READY 就回滚，落在这里。
    SwitchFailed(String),
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
        .map_err(|error| format!("Couldn't create {}: {error}", dir.display()))?;
    let launch = dir.join(daemon_image::LAUNCH_NAME);
    let image = daemon_image::publish_image(&dir, exe)
        .map_err(|error| format!("Couldn't publish the smeltd image: {error}"))?;
    // 包里带着 smelt-agent 就一起发布、拨链接，和守护换不换代无关。从 `~/.smelt/bin` 或
    // 开发目录 install 时旁边没有，交给运行时启动时按自己的版本补（`ensure_smelt_agent`）。
    let package = exe.parent().ok_or("smeltd has no parent directory")?;
    if smelt_core::agent_image::bundled(package) {
        smelt_core::agent_image::publish(&dir, package)
            .map_err(|error| format!("Couldn't publish smelt-agent: {error}"))?;
    }
    let installed = std::fs::canonicalize(&launch).ok() == std::fs::canonicalize(&image).ok();
    let running = smelt_runtime_client::local::local_runtime()
        .describe()
        .is_ok();
    let outcome = if installed {
        ImageInstall::AlreadyInstalled
    } else if running {
        daemon_image::retarget_launch_after_commit(&image)
            .map_err(|error| format!("Couldn't set the launch name: {error}"))?;
        match crate::request_self_upgrade(&image) {
            crate::SelfUpgradeOutcome::Upgraded => ImageInstall::Upgraded,
            crate::SelfUpgradeOutcome::Failed(error) => ImageInstall::SwitchFailed(format!(
                "Couldn't switch the runtime to the new version: {error}"
            )),
        }
    } else {
        daemon_image::retarget_launch_after_commit(&image)
            .map_err(|error| format!("Couldn't set the launch name: {error}"))?;
        ImageInstall::Launched
    };
    if let Err(error) = daemon_image::reap_unused_images(&dir) {
        eprintln!("Couldn't clean up the old image: {error}");
    }
    if let Err(error) = smelt_core::agent_image::reap_unused(&dir) {
        eprintln!("Couldn't clean up the old smelt-agent: {error}");
    }
    Ok(outcome)
}

/// `~/.smelt/bin`：启动名、映像和随运行时一起装的小工具都在这里。
#[cfg(target_os = "linux")]
pub(crate) fn bin_dir() -> Result<PathBuf, String> {
    Ok(smelt_paths::smelt_home()
        .ok_or_else(|| "Couldn't find $HOME".to_string())?
        .join("bin"))
}

/// 把正在执行的这份 smeltd 装好，返回启动名。
#[cfg(target_os = "linux")]
fn install_current_exe() -> Result<PathBuf, String> {
    let exe = std::env::current_exe()
        .map_err(|error| format!("Couldn't find smeltd's own path: {error}"))?;
    match install_image(&exe)? {
        ImageInstall::AlreadyInstalled => println!("This version is already installed"),
        ImageInstall::Upgraded => {
            println!(
                "The runtime restarted on the new version. Running turns were interrupted; history is kept"
            )
        }
        ImageInstall::Launched => {}
        // 安装器是另一个进程，停得了旧运行时：启动名已经拨好，看护（systemd 的
        // Restart=always，或 smelt-up）按它拉起的就是新版本。守护内部的远程升级
        // （runtime_upgrade）不走这里，那里停掉的是它自己。
        ImageInstall::SwitchFailed(error) => {
            println!(
                "{error}; stopping the old runtime instead so the supervisor starts the new version"
            );
            stop_running_daemon()?;
            println!(
                "The old runtime stopped; the supervisor will start the new version. Running turns were interrupted; history is kept"
            )
        }
    }
    Ok(bin_dir()?.join(smelt_core::daemon_image::LAUNCH_NAME))
}

/// 停机回执之后，运行时还要关 iroh、清会话目录才退出；等它退出的上限。
#[cfg(target_os = "linux")]
const STOP_EXIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

#[cfg(target_os = "linux")]
fn stop_running_daemon() -> Result<(), String> {
    stop_daemon(
        &smelt_runtime_client::local::local_runtime(),
        STOP_EXIT_TIMEOUT,
    )
}

/// 请运行时优雅停机，等它的进程真的退出。`shutdown` op 旧版本也认。
#[cfg(any(target_os = "linux", test))]
fn stop_daemon(
    runtime: &smelt_runtime_client::RuntimeClient,
    exit_timeout: std::time::Duration,
) -> Result<(), String> {
    use std::io::{BufRead, BufReader};

    use smelt_runtime_client::protocol::daemon::DaemonOperation;

    let pid = runtime
        .describe()
        .map_err(|error| format!("Couldn't reach the running runtime: {error}"))?
        .daemon
        .pid;
    let pid = i32::try_from(pid).map_err(|_| format!("The runtime reported a bad pid: {pid}"))?;
    let mut stream = runtime
        .connect_with_timeout(std::time::Duration::from_secs(5))
        .map_err(|error| format!("Couldn't connect to the running runtime: {error}"))?;
    let _ = stream.set_read_timeout(Some(exit_timeout));
    smelt_runtime_client::protocol::write_json_line(
        &mut stream,
        &serde_json::json!({ "op": DaemonOperation::Shutdown }),
    )
    .map_err(|error| format!("Couldn't ask the runtime to shut down: {error}"))?;
    // 先回执再清理、退出；读到断开也一样，退没退以进程为准。
    let mut reply = String::new();
    let _ = BufReader::new(&stream).read_line(&mut reply);
    if !smelt_sys::process_wait::wait_until_exit(pid, exit_timeout) {
        return Err(format!(
            "The old runtime (pid {pid}) didn't exit within {} seconds",
            exit_timeout.as_secs()
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn install() -> Result<(), String> {
    if !systemd_booted() {
        // 没有服务管理器可登记。已经在跑（smelt-up 拉起的）就照常交接到新映像。
        install_current_exe()?;
        println!(
            "This machine has no systemd, so no service was installed; smelt-up starts and watches the runtime"
        );
        return Ok(());
    }
    // 已经在跑、却不归 systemd 管（手动起的）：收编不进来，装上去也只是两个实例抢锁。
    if smelt_runtime_client::local::local_runtime()
        .describe()
        .is_ok()
        && !unit_active()
    {
        return Err(
            "The runtime is running outside systemd (see smeltd status for the pid). Stop it before installing; its sessions will end"
                .to_string(),
        );
    }
    let exe = install_current_exe()?;
    let unit = unit_path()?;
    if let Some(dir) = unit.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|error| format!("Couldn't create {}: {error}", dir.display()))?;
    }
    std::fs::write(&unit, unit_contents(&exe))
        .map_err(|error| format!("Couldn't write {}: {error}", unit.display()))?;
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", "--now", UNIT_NAME])?;
    println!("Installed and started: {}", unit.display());
    if !linger_enabled() {
        println!(
            "Note: systemd stops user services when you log out. To keep the runtime online, run:\n  loginctl enable-linger {}",
            whoami()
        );
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn uninstall() -> Result<(), String> {
    if !systemd_booted() {
        return Err(
            "This machine has no systemd, so there's no service to remove; smelt-up watches the runtime, so stop that instead".to_string(),
        );
    }
    let unit = unit_path()?;
    // 服务可能已经停了或从没装过，停和禁用失败都不算错。
    let _ = systemctl(&["disable", "--now", UNIT_NAME]);
    match std::fs::remove_file(&unit) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("Couldn't delete {}: {error}", unit.display())),
    }
    systemctl(&["daemon-reload"])?;
    println!("Removed {UNIT_NAME}");
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
    Err("Only Linux needs the service installed by hand; on macOS the Smelt desktop starts the runtime".to_string())
}

#[cfg(not(target_os = "linux"))]
fn uninstall() -> Result<(), String> {
    Err("Only Linux has a systemd service to remove".to_string())
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

    /// 假运行时：`system.describe` 报 `pid`，收到 `shutdown` 先回执，再按 `exits` 决定
    /// 杀不杀掉那个进程。记下收到的 op。
    fn fake_runtime(
        pid: u32,
        child: std::sync::Arc<std::sync::Mutex<Option<std::process::Child>>>,
        exits: bool,
        ops: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) -> smelt_runtime_client::RuntimeClient {
        use std::io::{BufRead, BufReader};

        use smelt_runtime_client::RuntimeStream;
        use smelt_runtime_client::protocol::control::ControlResponse;
        use smelt_runtime_client::protocol::write_json_line;

        smelt_runtime_client::RuntimeClient::new(move || {
            let (client, server) = RuntimeStream::pair()?;
            let child = std::sync::Arc::clone(&child);
            let ops = std::sync::Arc::clone(&ops);
            std::thread::spawn(move || {
                let mut line = String::new();
                if BufReader::new(&server).read_line(&mut line).is_err() {
                    return;
                }
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                let op = request["op"].as_str().unwrap_or_default().to_string();
                ops.lock().unwrap().push(op.clone());
                match op.as_str() {
                    "control" => {
                        let request_id = request["request_id"].as_str().unwrap_or_default();
                        let described = serde_json::json!({
                            "control": {
                                "current_version": 1,
                                "min_supported_version": 1,
                                "max_supported_version": 1,
                                "methods": ["system.describe"],
                            },
                            "daemon": {
                                "version": "2026.1023.0-rc.1",
                                "exe_mtime": 0,
                                "exe": null,
                                "pid": pid,
                                "started_at": 0,
                                "session_count": 0,
                                "agent_event_version": 1,
                            },
                        });
                        let _ = write_json_line(
                            &mut &server,
                            &ControlResponse::ok(request_id, described),
                        );
                    }
                    "shutdown" => {
                        let _ = write_json_line(&mut &server, &serde_json::json!({ "ok": true }));
                        if exits && let Some(mut child) = child.lock().unwrap().take() {
                            let _ = child.kill();
                            let _ = child.wait();
                        }
                    }
                    other => panic!("假运行时不认识的 op：{other}"),
                }
            });
            Ok(client)
        })
    }

    /// 旧运行时换不了代时，安装器请它停机，等它的进程真的退出，看护才会按新启动名拉起。
    #[test]
    fn stop_daemon_shuts_the_runtime_down_and_waits_for_its_process() {
        let child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        let child = std::sync::Arc::new(std::sync::Mutex::new(Some(child)));
        let ops = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let runtime = fake_runtime(pid, std::sync::Arc::clone(&child), true, ops.clone());

        assert_eq!(
            stop_daemon(&runtime, std::time::Duration::from_secs(5)),
            Ok(())
        );
        assert_eq!(*ops.lock().unwrap(), ["control", "shutdown"]);
        assert!(child.lock().unwrap().is_none(), "进程应已被停掉");
    }

    #[test]
    fn stop_daemon_fails_when_the_runtime_does_not_exit() {
        let child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        let child = std::sync::Arc::new(std::sync::Mutex::new(Some(child)));
        let ops = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let runtime = fake_runtime(pid, std::sync::Arc::clone(&child), false, ops);

        let error = stop_daemon(&runtime, std::time::Duration::from_millis(200)).unwrap_err();

        assert!(error.contains("didn't exit"), "{error}");
        if let Some(mut child) = child.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
