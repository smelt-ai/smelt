//! macOS App 安装 helper，两种入口共用同一条提交顺序（等 GUI 退出 → 停守护 → 交换 App）：
//!
//! - 在线更新：GUI 派生一次性副本，命令行只带不透明事务凭据；候选和目标路径都由
//!   updater core 从客户端库读取并校验。
//! - `--local-app <Smelt.app> [--target <路径>]`：`make install` 装本地构建的包。前台复制
//!   好副本，再把提交交给新会话里的 `--commit-local-app <id> --target <路径>`，等它结束。

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn argument(args: &[String], name: &str) -> anyhow::Result<String> {
    let index = args
        .iter()
        .position(|arg| arg == name)
        .ok_or_else(|| anyhow::anyhow!("缺少参数 {name}"))?;
    args.get(index + 1)
        .filter(|value| !value.starts_with("--"))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("参数 {name} 缺少值"))
}

#[cfg(target_os = "macos")]
fn run() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--local-app") => return run_local(&args),
        Some("--commit-local-app") => return run_commit_local(&args),
        _ => {}
    }
    let update_id = argument(&args, "--update-id")?;
    let attempt_id = argument(&args, "--attempt-id")?;
    if args.len() != 4 {
        anyhow::bail!("用法：smelt-installer --update-id <id> --attempt-id <id>");
    }
    smelt_client::updater::run_installer(&update_id, &attempt_id)
}

#[cfg(target_os = "macos")]
fn run_local(args: &[String]) -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt as _;
    use std::path::Path;

    let source = argument(args, "--local-app")?;
    let target = match args.len() {
        2 => "/Applications/Smelt.app".to_string(),
        4 => argument(args, "--target")?,
        _ => anyhow::bail!("用法：smelt-installer --local-app <Smelt.app> [--target <Smelt.app>]"),
    };
    let target_path = Path::new(&target);
    let id = smelt_client::updater::stage_local_app(Path::new(&source), target_path)?;

    // 提交时要停守护，守护会杀掉它名下终端和 ACP 的进程组。这个进程可能就在那样的终端
    // 里，所以提交放进新会话：不在那些进程组里，也没有会在终端关掉时收到 SIGHUP 的控制终端。
    // 它看不到终端，进度和错误写 app.log；这边只等它结束、报结果，这边被杀了它也照样做完。
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command
        .args(["--commit-local-app", &id, "--target", &target])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // SAFETY: fork 之后只调用异步信号安全的 setsid。
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let _ = smelt_client::updater::discard_local_app(target_path, &id);
            return Err(anyhow::anyhow!("启动安装进程失败：{error}"));
        }
    };
    eprintln!("· 等 Smelt 退出…");
    let status = child.wait()?;
    if !status.success() {
        let log = smelt_sys::app_log::log_path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "app.log".to_string());
        anyhow::bail!("安装没有完成（{status}），原因见 {log}");
    }
    println!("✅ 已安装 {target}");
    Ok(())
}

#[cfg(target_os = "macos")]
fn run_commit_local(args: &[String]) -> anyhow::Result<()> {
    let id = argument(args, "--commit-local-app")?;
    let target = argument(args, "--target")?;
    if args.len() != 4 {
        anyhow::bail!("用法：smelt-installer --commit-local-app <id> --target <Smelt.app>");
    }
    smelt_client::updater::commit_local_app(std::path::Path::new(&target), &id)
}

// 更新器只替换 macOS 的 .app 包，别的平台没有可装的东西。
#[cfg(not(target_os = "macos"))]
fn run() -> anyhow::Result<()> {
    anyhow::bail!("smelt-installer 只用于 macOS 桌面更新")
}

fn main() {
    if let Err(error) = run() {
        smelt_sys::app_log::error("installer", &format!("App 更新安装失败：{error:#}"));
        eprintln!("smelt-installer: {error:#}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::argument;

    #[test]
    fn only_reads_named_opaque_arguments() {
        let args = vec![
            "--attempt-id".to_string(),
            "attempt-1".to_string(),
            "--update-id".to_string(),
            "update-1".to_string(),
        ];
        assert_eq!(argument(&args, "--update-id").unwrap(), "update-1");
        assert_eq!(argument(&args, "--attempt-id").unwrap(), "attempt-1");
        assert!(argument(&args, "--target-app").is_err());
    }
}
