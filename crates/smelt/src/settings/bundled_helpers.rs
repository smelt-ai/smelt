//! 本机运行时的随包 helper：`smelt-notify`（agent hooks 调用）。
//!
//! 这是「安装本机运行时」的一步，和 smeltd 镜像一样由桌面从 App 包里放到
//! `~/.smelt/bin`。远程运行时由它自己的安装包放好。

/// smelt-notify 安装路径（与 package/安装脚本约定一致）。
fn smelt_notify_path() -> std::path::PathBuf {
    smelt_model::agent_event::notify_executable_path()
}

/// 把 App / cargo 产物旁的 `smelt-notify` 原子同步到 hooks 使用的稳定路径。
///
/// hooks 会在 GUI 关闭时运行，不能直接指向可能被 DMG 覆盖的 App 包。每次启动都覆盖
/// managed 副本，避免“hook JSON 已升级、helper 仍是旧版”的半升级状态。rename 替换
/// 不影响已经启动的旧 helper：它仍持有旧 inode，下一次 hook 自动使用新文件。
pub fn sync_bundled_smelt_notify() -> std::io::Result<()> {
    let bundled = std::env::current_exe()?.with_file_name(if cfg!(windows) {
        "smelt-notify.exe"
    } else {
        "smelt-notify"
    });
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
    let staged = dir.join(if cfg!(windows) {
        "smelt-notify.exe.next"
    } else {
        "smelt-notify.next"
    });
    let _ = std::fs::remove_file(&staged);
    std::fs::copy(&bundled, &staged)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&staged)?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&staged, permissions)?;
    }
    // 正在跑的 exe 映射着目标文件时，Windows 上直接 rename 覆盖会失败。
    // 先把旧文件挪开，再把新文件换进去。
    if std::fs::rename(&staged, &managed).is_err() {
        let aside = dir.join(if cfg!(windows) {
            "smelt-notify.exe.old"
        } else {
            "smelt-notify.old"
        });
        let _ = std::fs::remove_file(&aside);
        std::fs::rename(&managed, &aside)?;
        if let Err(error) = std::fs::rename(&staged, &managed) {
            let _ = std::fs::rename(&aside, &managed);
            return Err(error);
        }
        let _ = std::fs::remove_file(&aside);
    }
    Ok(())
}
