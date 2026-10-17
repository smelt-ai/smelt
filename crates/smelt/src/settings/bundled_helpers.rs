//! 本机运行时的随包 helper：`smelt-notify`（agent hooks 调用）和 `smelt-agent-mcp`
//! （跨 agent 消息）。
//!
//! 这是「安装本机运行时」的一步，和 smeltd 镜像一样由桌面从 App 包里放到
//! `~/.smelt/bin`。远程运行时由它自己的安装包放好。

/// smelt-notify 安装路径（与 package/安装脚本约定一致）。
fn smelt_notify_path() -> std::path::PathBuf {
    smelt_model::agent_event::notify_executable_path()
}

/// cross-agent MCP helper 的稳定安装路径。managed smeltd 与它同目录，ACP/PTY
/// 注入都可从守护可执行文件旁直接定位，不依赖用户 PATH。
pub fn smelt_agent_mcp_path() -> std::path::PathBuf {
    smelt_paths::smelt_home()
        .unwrap_or_else(|| "/tmp/.smelt".into())
        .join("bin")
        .join("smelt-agent-mcp")
}

/// 把 App / cargo 产物旁的 `smelt-notify` 原子同步到 hooks 使用的稳定路径。
///
/// hooks 会在 GUI 关闭时运行，不能直接指向可能被 DMG 覆盖的 App 包。每次启动都覆盖
/// managed 副本，避免“hook JSON 已升级、helper 仍是旧版”的半升级状态。rename 替换
/// 不影响已经启动的旧 helper：它仍持有旧 inode，下一次 hook 自动使用新文件。
pub fn sync_bundled_smelt_notify() -> std::io::Result<()> {
    let bundled = std::env::current_exe()?.with_file_name("smelt-notify");
    if !bundled.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("App 包内缺少 {}", bundled.display()),
        ));
    }

    let managed = smelt_notify_path();
    let dir = managed.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("无效的 smelt-notify 路径：{}", managed.display()),
        )
    })?;
    std::fs::create_dir_all(dir)?;
    let staged = dir.join("smelt-notify.next");
    let _ = std::fs::remove_file(&staged);
    std::fs::copy(&bundled, &staged)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&staged)?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&staged, permissions)?;
    }
    std::fs::rename(staged, managed)
}

/// 把 App / cargo 产物旁的 cross-agent MCP helper 原子同步到 managed 目录。
pub fn sync_bundled_smelt_agent_mcp() -> std::io::Result<()> {
    let bundled = std::env::current_exe()?.with_file_name("smelt-agent-mcp");
    if !bundled.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("App 包内缺少 {}", bundled.display()),
        ));
    }

    let managed = smelt_agent_mcp_path();
    let dir = managed.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("无效的 smelt-agent-mcp 路径：{}", managed.display()),
        )
    })?;
    std::fs::create_dir_all(dir)?;
    let staged = dir.join("smelt-agent-mcp.next");
    let _ = std::fs::remove_file(&staged);
    std::fs::copy(&bundled, &staged)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&staged)?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&staged, permissions)?;
    }
    std::fs::rename(staged, managed)
}
