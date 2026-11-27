//! macOS App 安装 helper，两种入口共用同一条换包的路（等 GUI 退出 → 停守护 → 交换 App）：
//!
//! - `--install <下好的.app> --target <App> --replacing <版本> [--relaunch]`：在线更新。GUI
//!   把自己拷到下好的包旁边再拉起它，换包前再校验一遍签名身份；`target` 已经不是 `--replacing`
//!   这个版本（被别的途径换掉了）就不装。
//! - `--local-app <Smelt.app> [--target <路径>]`：`make install` 装本地构建的包。前台复制
//!   好副本，再把提交交给新会话里的 `--commit-local-app <id> --target <路径>`，等它结束。

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn argument(args: &[String], name: &str) -> anyhow::Result<String> {
    let index = args
        .iter()
        .position(|arg| arg == name)
        .ok_or_else(|| anyhow::anyhow!("missing argument {name}"))?;
    args.get(index + 1)
        .filter(|value| !value.starts_with("--"))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("argument {name} has no value"))
}

#[cfg(target_os = "macos")]
fn run() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--install") => run_install(&args),
        Some("--local-app") => run_local(&args),
        Some("--commit-local-app") => run_commit_local(&args),
        _ => anyhow::bail!(
            "usage: smelt-installer --install <Smelt.app> --target <Smelt.app> --replacing <version> [--relaunch]"
        ),
    }
}

#[cfg(target_os = "macos")]
fn run_install(args: &[String]) -> anyhow::Result<()> {
    use std::path::Path;

    let staged = argument(args, "--install")?;
    let target = argument(args, "--target")?;
    let replacing = argument(args, "--replacing")?;
    let relaunch = args.iter().any(|arg| arg == "--relaunch");
    if args.len() != if relaunch { 7 } else { 6 } {
        anyhow::bail!(
            "usage: smelt-installer --install <Smelt.app> --target <Smelt.app> --replacing <version> [--relaunch]"
        );
    }
    smelt_client::updater::install_update(
        Path::new(&staged),
        Path::new(&target),
        &replacing,
        relaunch,
    )
}

#[cfg(target_os = "macos")]
fn run_local(args: &[String]) -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt as _;
    use std::path::Path;

    let source = argument(args, "--local-app")?;
    let target = match args.len() {
        2 => "/Applications/Smelt.app".to_string(),
        4 => argument(args, "--target")?,
        _ => anyhow::bail!("usage: smelt-installer --local-app <Smelt.app> [--target <Smelt.app>]"),
    };
    let target_path = Path::new(&target);
    let id = smelt_client::updater::stage_local_app(Path::new(&source), target_path)?;

    // 提交时要停守护，守护会杀掉它名下终端和 ACP 的进程组。这个进程可能就在那样的终端
    // 里，所以提交放进新会话：不在那些进程组里，也没有会在终端关掉时收到 SIGHUP 的控制终端。
    // 它看不到终端，进度和错误写 app.log；这边只等它结束、报结果，这边被杀了它也照样做完。
    let mut command = std::process::Command::new(smelt_paths::current_exe()?);
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
            return Err(anyhow::anyhow!("couldn't start the installer: {error}"));
        }
    };
    eprintln!("· 等 Smelt 退出…");
    let status = child.wait()?;
    if !status.success() {
        let log = smelt_sys::app_log::log_path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "app.log".to_string());
        anyhow::bail!("the install didn't finish ({status}); see {log}");
    }
    println!("✅ installed {target}");
    Ok(())
}

#[cfg(target_os = "macos")]
fn run_commit_local(args: &[String]) -> anyhow::Result<()> {
    let id = argument(args, "--commit-local-app")?;
    let target = argument(args, "--target")?;
    if args.len() != 4 {
        anyhow::bail!("usage: smelt-installer --commit-local-app <id> --target <Smelt.app>");
    }
    smelt_client::updater::commit_local_app(std::path::Path::new(&target), &id)
}

// 更新器只替换 macOS 的 .app 包，别的平台没有可装的东西。
#[cfg(not(target_os = "macos"))]
fn run() -> anyhow::Result<()> {
    anyhow::bail!("smelt-installer is only for macOS desktop updates")
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
    fn only_reads_named_arguments_with_values() {
        let args: Vec<String> = [
            "--install",
            "/cache/a/Smelt.app",
            "--target",
            "--relaunch",
            "--replacing",
            "2026.1102.0",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(argument(&args, "--install").unwrap(), "/cache/a/Smelt.app");
        assert_eq!(argument(&args, "--replacing").unwrap(), "2026.1102.0");
        assert!(argument(&args, "--target").is_err(), "值不能是下一个参数");
        assert!(argument(&args, "--local-app").is_err());
    }
}
