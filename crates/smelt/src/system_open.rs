//! 交给系统打开：在文件管理器里打开目录、用系统文本编辑器打开文件。
//!
//! 宿主经 `smelt.app/open-in-file-manager`、`smelt.app/open-in-text-editor` 把这两样提供给
//! 插件（见 plugin_host.rs）。只把请求交给系统，不等对方退出；参数始终经 `Command` 传，
//! 不过 shell，路径里的空格和特殊字符不会被错误解释。

use std::path::Path;
use std::process::{Command, Stdio};

/// 在系统文件管理器中打开目录。该函数不等待文件管理器退出。
pub(crate) fn open_in_file_manager(path: &Path) -> Result<(), String> {
    if !path.is_dir() {
        return Err(format!("项目目录不存在或不可访问：{}", path.display()));
    }

    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("open");
        command.arg(path);
        spawn_detached(&mut command, "Finder")
    }

    #[cfg(target_os = "windows")]
    {
        let mut command = Command::new("explorer");
        command.arg(path);
        return spawn_detached(&mut command, "文件管理器");
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut command = Command::new("xdg-open");
        command.arg(path);
        return spawn_detached(&mut command, "文件管理器");
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", unix)))]
    Err("当前平台不支持打开文件管理器".to_string())
}

/// 用系统默认的文本编辑器打开文件，不经工作台的文件面板：设置页用它，在工作台里打开会
/// 把人从正在改的设置页带走。
pub(crate) fn open_in_system_text_editor(path: &Path) -> Result<(), String> {
    if !path.is_file() {
        return Err(format!("配置文件不存在或不可访问：{}", path.display()));
    }

    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("open");
        command.arg("-t").arg(path);
        spawn_detached(&mut command, "系统文本编辑器")
    }

    #[cfg(target_os = "windows")]
    {
        let mut command = Command::new("notepad");
        command.arg(path);
        return spawn_detached(&mut command, "系统文本编辑器");
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut command = Command::new("xdg-open");
        command.arg(path);
        return spawn_detached(&mut command, "系统文本编辑器");
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", unix)))]
    Err("当前平台不支持打开文本编辑器".to_string())
}

fn spawn_detached(command: &mut Command, application: &str) -> Result<(), String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("启动 {application} 失败：{error}"))
}
