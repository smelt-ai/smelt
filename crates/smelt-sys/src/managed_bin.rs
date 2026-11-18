//! `~/.smelt/bin` 的整理：守护启动时调一次 [`tidy`]。
//!
//! 2026-11-18 之前 smeltd 装成 `smeltd-<指纹>`、smelt-agent 装成 `smelt-agent-<指纹>/`，启动名
//! 是指向它们的链接（Windows 上是指针文件），升级时新旧版本并存，靠探测有没有进程映射来回收。
//! 现在名字固定（见 [`crate::daemon_image`]、[`crate::agent_image`]）。这里把旧布局迁过来、删掉
//! 旧布局和装到一半留下的临时文件。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::{agent_image, daemon_image};

/// 先迁再删。在守护起任何 Pi 之前调：迁移要挪 smelt-agent 的目录。删不掉的（Windows 上还有
/// 进程占着）留到下次。返回遇到的第一个错误，其余照做。
pub fn tidy(dir: &Path) -> io::Result<()> {
    let mut first_error = None;
    let mut keep = |result: io::Result<()>| {
        if let Err(error) = result
            && error.kind() != io::ErrorKind::NotFound
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    };
    keep(migrate_daemon(dir));
    keep(migrate_agent(dir));
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !is_leftover(&name) {
            continue;
        }
        let path = entry.path();
        keep(
            if fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_dir()) {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            },
        );
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// 旧布局的启动名还指着 `smeltd-<指纹>`：把那份复制成固定名字的文件。Linux 上从旧版本升上来
/// 的守护就是从 `smeltd-<指纹>` 起的，systemd 下次按启动名拉起时要能找到它。
fn migrate_daemon(dir: &Path) -> io::Result<()> {
    let Some(image) = legacy_daemon_image(dir) else {
        return Ok(());
    };
    daemon_image::install(dir, &image).map(drop)
}

#[cfg(unix)]
fn legacy_daemon_image(dir: &Path) -> Option<PathBuf> {
    let launch = dir.join(daemon_image::LAUNCH_NAME);
    fs::symlink_metadata(&launch)
        .ok()
        .filter(|meta| meta.file_type().is_symlink())?;
    fs::canonicalize(&launch).ok().filter(|path| path.is_file())
}

/// Windows 上旧的启动名是没有扩展名的指针文件，`smeltd.exe` 还没装时才从它迁。
#[cfg(windows)]
fn legacy_daemon_image(dir: &Path) -> Option<PathBuf> {
    if daemon_image::installed_path(dir).is_file() {
        return None;
    }
    read_pointer(&dir.join(daemon_image::LAUNCH_NAME))
}

#[cfg(not(any(unix, windows)))]
fn legacy_daemon_image(_dir: &Path) -> Option<PathBuf> {
    None
}

/// 还没有 `agent/` 时，把旧链接指着的那个 `smelt-agent-<指纹>/` 改名过来。没有指纹记录，
/// 下次装时会整套重装一遍。
fn migrate_agent(dir: &Path) -> io::Result<()> {
    if fs::symlink_metadata(dir.join(agent_image::INSTALL_DIR)).is_ok() {
        return Ok(());
    }
    let Some(legacy) = legacy_agent_dir(dir) else {
        return Ok(());
    };
    fs::rename(&legacy, dir.join(agent_image::INSTALL_DIR))?;
    #[cfg(unix)]
    {
        let link = dir.join(agent_image::LAUNCH_NAME);
        let tmp = dir.join(format!(
            ".{}.link-{}",
            agent_image::LAUNCH_NAME,
            std::process::id()
        ));
        let target = Path::new(agent_image::INSTALL_DIR).join(agent_image::binary_file_name());
        let result = std::os::unix::fs::symlink(target, &tmp).and_then(|()| fs::rename(&tmp, link));
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result?;
    }
    Ok(())
}

#[cfg(unix)]
fn legacy_agent_dir(dir: &Path) -> Option<PathBuf> {
    let executable = fs::canonicalize(dir.join(agent_image::LAUNCH_NAME)).ok()?;
    let parent = executable.parent()?;
    let name = parent.file_name()?.to_str()?;
    (parent.parent() == fs::canonicalize(dir).ok().as_deref()
        && is_fingerprinted(name, "smelt-agent-"))
    .then(|| parent.to_path_buf())
}

#[cfg(windows)]
fn legacy_agent_dir(dir: &Path) -> Option<PathBuf> {
    let executable = read_pointer(&dir.join("smelt-agent.current"))?;
    let parent = executable.parent()?;
    let name = parent.file_name()?.to_str()?;
    is_fingerprinted(name, "smelt-agent-").then(|| parent.to_path_buf())
}

#[cfg(not(any(unix, windows)))]
fn legacy_agent_dir(_dir: &Path) -> Option<PathBuf> {
    None
}

/// 旧的指针文件：首行 `smelt-image-v1`，下一行是相对它所在目录的路径。
#[cfg(windows)]
fn read_pointer(pointer: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(pointer).ok()?;
    let mut lines = text.lines();
    if lines.next()? != "smelt-image-v1" {
        return None;
    }
    let mut path = pointer.parent()?.to_path_buf();
    for part in lines.next()?.split(['/', '\\']) {
        if !part.is_empty() && part != "." && part != ".." {
            path.push(part);
        }
    }
    path.is_file().then_some(path)
}

/// 迁完之后可以删的：旧布局的映像、目录、链接和指针，以及装到一半留下的临时文件。
fn is_leftover(name: &str) -> bool {
    let fingerprinted = is_fingerprinted(name.trim_end_matches(".exe"), "smeltd-")
        || is_fingerprinted(name, "smelt-agent-")
        || is_fingerprinted(name, "smelt-pi-");
    let pointers = [
        "smeltd.pending",
        "smelt-agent.current",
        "smelt-pi",
        "smelt-pi.current",
        // 跨 Agent 消息下线后的 MCP helper。
        "smelt-agent-mcp",
        "smelt-agent-mcp.next",
    ];
    // Windows 上没有扩展名的 `smeltd` 是旧的指针文件；Unix 上它就是守护本身。
    let windows_pointer = cfg!(windows) && name == daemon_image::LAUNCH_NAME;
    let temporary = [".smeltd", ".smelt-agent", ".smelt-pi", ".agent."]
        .iter()
        .any(|prefix| name.starts_with(prefix));
    fingerprinted || pointers.contains(&name) || windows_pointer || temporary
}

fn is_fingerprinted(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix).is_some_and(|rest| {
        rest.len() == 64
            && rest
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "smelt-managed-bin-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 从旧版本升上来的 Linux 守护：启动名是链接、smelt-agent 在带指纹的目录里。整理之后
    /// 都换成固定名字，旧东西删干净，别的文件不碰。
    #[test]
    fn the_fingerprinted_layout_moves_to_fixed_names() {
        let bin = scratch();
        let fp = |c: char| c.to_string().repeat(64);
        let image = bin.join(format!("smeltd-{}", fp('a')));
        fs::write(&image, b"daemon").unwrap();
        fs::write(bin.join(format!("smeltd-{}", fp('b'))), b"older daemon").unwrap();
        std::os::unix::fs::symlink(image.file_name().unwrap(), bin.join("smeltd")).unwrap();
        std::os::unix::fs::symlink(image.file_name().unwrap(), bin.join("smeltd.pending")).unwrap();
        let agent = bin.join(format!("smelt-agent-{}", fp('c')));
        fs::create_dir_all(&agent).unwrap();
        fs::write(agent.join("smelt-agent"), b"agent").unwrap();
        let stale_agent = bin.join(format!("smelt-agent-{}", fp('d')));
        fs::create_dir_all(&stale_agent).unwrap();
        std::os::unix::fs::symlink(
            Path::new(agent.file_name().unwrap()).join("smelt-agent"),
            bin.join("smelt-agent"),
        )
        .unwrap();
        fs::create_dir_all(bin.join(format!(".smelt-agent-{}.partial-1-x", fp('e')))).unwrap();
        fs::write(bin.join("smelt-notify"), b"helper").unwrap();
        fs::create_dir_all(bin.join("smelt-agent-notes")).unwrap();
        fs::write(bin.join("smelt-agent-mcp"), b"retired helper").unwrap();

        tidy(&bin).unwrap();

        let daemon = fs::symlink_metadata(bin.join("smeltd")).unwrap();
        assert!(daemon.file_type().is_file(), "启动名换成了文件");
        assert_eq!(fs::read(bin.join("smeltd")).unwrap(), b"daemon");
        assert_eq!(
            fs::read(bin.join("agent/smelt-agent")).unwrap(),
            b"agent",
            "当前那套 smelt-agent 挪进了 agent/"
        );
        assert_eq!(
            fs::canonicalize(bin.join("smelt-agent")).unwrap(),
            fs::canonicalize(bin.join("agent/smelt-agent")).unwrap()
        );
        let mut names: Vec<String> = fs::read_dir(&bin)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "agent",
                "smelt-agent",
                "smelt-agent-notes",
                "smelt-notify",
                "smeltd"
            ]
        );
        fs::remove_dir_all(bin).unwrap();
    }

    /// 已经是新布局：什么都不动。
    #[test]
    fn the_fixed_layout_is_left_alone() {
        let bin = scratch();
        let src = bin.join("src");
        fs::write(&src, b"daemon").unwrap();
        daemon_image::install(&bin, &src).unwrap();
        fs::remove_file(&src).unwrap();
        fs::create_dir_all(bin.join("agent")).unwrap();
        fs::write(bin.join("agent/smelt-agent"), b"agent").unwrap();
        std::os::unix::fs::symlink("agent/smelt-agent", bin.join("smelt-agent")).unwrap();

        tidy(&bin).unwrap();

        assert_eq!(fs::read(bin.join("smeltd")).unwrap(), b"daemon");
        assert_eq!(fs::read(bin.join("smelt-agent")).unwrap(), b"agent");
        fs::remove_dir_all(bin).unwrap();
    }
}
