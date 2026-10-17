//! 一次性 macOS App 安装 helper。
//!
//! 命令行只接受不透明事务凭据；候选、目标路径和父 GUI PID 均由共享 updater core
//! 从客户端库读取并校验，避免把 helper 变成任意路径替换器。

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
    let update_id = argument(&args, "--update-id")?;
    let attempt_id = argument(&args, "--attempt-id")?;
    if args.len() != 4 {
        anyhow::bail!("用法：smelt-installer --update-id <id> --attempt-id <id>");
    }
    smelt_client::updater::run_installer(&update_id, &attempt_id)
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
