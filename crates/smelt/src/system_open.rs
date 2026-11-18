//! 交给系统打开：在文件管理器里打开目录、用系统文本编辑器打开文件。
//!
//! 宿主经 `smelt.app/open-in-file-manager`、`smelt.app/open-in-text-editor` 把这两样提供给
//! 插件（见 plugin_host.rs）。只把请求交给系统，不等对方退出；参数始终经 `Command` 传，
//! 不过 shell，路径里的空格和特殊字符不会被错误解释。

use rust_i18n::t;
use std::borrow::Cow;
use std::path::Path;
use std::process::{Command, Stdio};

/// 在系统文件管理器中打开目录。该函数不等待文件管理器退出。
pub(crate) fn open_in_file_manager(path: &Path) -> Result<(), String> {
    if !path.is_dir() {
        return Err(t!("open.no_dir", path = path.display()).into_owned());
    }

    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("open");
        command.arg(path);
        spawn_detached(&mut command, "Finder".into())
    }

    #[cfg(target_os = "windows")]
    {
        let mut command = Command::new("explorer");
        command.arg(path);
        return spawn_detached(&mut command, t!("open.file_manager"));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut command = Command::new("xdg-open");
        command.arg(path);
        return spawn_detached(&mut command, t!("open.file_manager"));
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", unix)))]
    Err(t!("open.no_file_manager").into_owned())
}

/// 用系统默认的文本编辑器打开文件，不经工作台的文件面板：设置页用它，在工作台里打开会
/// 把人从正在改的设置页带走。
pub(crate) fn open_in_system_text_editor(path: &Path) -> Result<(), String> {
    if !path.is_file() {
        return Err(t!("open.no_file", path = path.display()).into_owned());
    }

    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("open");
        command.arg("-t").arg(path);
        spawn_detached(&mut command, t!("open.text_editor"))
    }

    #[cfg(target_os = "windows")]
    {
        let mut command = Command::new("notepad");
        command.arg(path);
        return spawn_detached(&mut command, t!("open.text_editor"));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut command = Command::new("xdg-open");
        command.arg(path);
        return spawn_detached(&mut command, t!("open.text_editor"));
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", unix)))]
    Err(t!("open.no_text_editor").into_owned())
}

fn spawn_detached(command: &mut Command, application: Cow<'static, str>) -> Result<(), String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| {
            t!(
                "open.spawn_failed",
                application = application,
                error = error
            )
            .into_owned()
        })
}
