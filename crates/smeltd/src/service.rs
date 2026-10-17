//! smeltd 自己的命令行：`install`、`uninstall`、`status`、`pair`、`devices`、`--version`。
//!
//! 无头机器上没有桌面来拉起守护，运行时要能自己装成常驻服务（设计文档 D7）。Linux 用
//! systemd 用户单元：跟着这个用户的登录会话走，`loginctl enable-linger` 之后退出登录
//! 也继续跑。macOS 上守护照旧由桌面拉起，这里只提示。

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
  smeltd install     装成 systemd 用户服务并立即启动（Linux）
  smeltd uninstall   停止并移除 systemd 用户服务（Linux）
  smeltd status      查看本机运行时是否在跑
  smeltd pair        开启远程访问，打印一次性配对串和二维码
  smeltd devices     列出已配对设备
  smeltd devices revoke <endpoint-id>
                     撤销一台设备，立即断开它
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
    local()
        .call::<C>(params, smelt_runtime_client::DEFAULT_REQUEST_TIMEOUT)
        .map_err(|error| format!("调用本机运行时失败：{error}"))
}

/// 远程访问没开就打开（保留已配的自建 relay），等它真正跑起来。
fn ensure_remote_running() -> Result<(), String> {
    use smelt_core::control_api::{RemoteConfigGet, RemoteConfigSet, RemoteStatus};
    use smelt_core::remote_config::RemoteAccessState;
    use smelt_runtime_client::protocol::control::EmptyParams;

    let mut state = call::<RemoteStatus>(&EmptyParams::default())?;
    if matches!(state, RemoteAccessState::Off | RemoteAccessState::Failed { .. }) {
        let mut config = call::<RemoteConfigGet>(&EmptyParams::default())?;
        if !config.enabled {
            config.enabled = true;
            println!("正在开启远程访问…");
            state = call::<RemoteConfigSet>(&config)?;
        }
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        match state {
            RemoteAccessState::Running { .. } => return Ok(()),
            RemoteAccessState::Failed { error } if std::time::Instant::now() >= deadline => {
                return Err(format!("远程访问起不来：{error}"));
            }
            _ if std::time::Instant::now() >= deadline => {
                return Err("等远程访问启动超时（60s），稍后再试".to_string());
            }
            _ => {}
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
        state = call::<RemoteStatus>(&EmptyParams::default())?;
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

#[cfg(target_os = "linux")]
fn unit_path() -> Result<PathBuf, String> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
        .ok_or_else(|| "找不到用户配置目录".to_string())?;
    Ok(config.join("systemd/user").join(UNIT_NAME))
}

#[cfg(any(target_os = "linux", test))]
/// 单元文件内容。`ExecStart` 用解析过软链的绝对路径：升级时 smeltd 在原进程里 exec
/// 新映像，systemd 只管这一个进程。`KillMode=process`：停服务只停守护本身，会话里的
/// agent 子进程由守护自己收拾，避免 systemd 连同终端里的进程一起杀掉。
fn unit_contents(exe: &Path) -> String {
    format!(
        "[Unit]
Description=Smelt 运行时（smeltd）
After=network-online.target

[Service]
Type=simple
ExecStart={exe}
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

#[cfg(target_os = "linux")]
fn install() -> Result<(), String> {
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|error| format!("找不到 smeltd 自身路径：{error}"))?;
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
