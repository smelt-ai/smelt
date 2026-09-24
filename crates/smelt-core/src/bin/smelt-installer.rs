//! 一次性 macOS App 安装 helper。
//!
//! 命令行只接受不透明事务凭据；候选、目标路径和父 GUI PID 均由共享 updater core
//! 从 SQLite 读取并校验，避免把 helper 变成任意路径替换器。

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

fn run() -> anyhow::Result<()> {
    smelt_core::sqlite_state::enable_sqlite_state();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let update_id = argument(&args, "--update-id")?;
    let attempt_id = argument(&args, "--attempt-id")?;
    if args.len() != 4 {
        anyhow::bail!("用法：smelt-installer --update-id <id> --attempt-id <id>");
    }
    smelt_core::updater::run_installer(&update_id, &attempt_id)
}

fn main() {
    if let Err(error) = run() {
        smelt_core::app_log::error("installer", &format!("App 更新安装失败：{error:#}"));
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
