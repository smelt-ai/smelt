//! 磁盘小工具：不跟随软链的类型、真实路径、建删软链、整树复制、原子交换、移进回收目录。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Symlink,
    Dir,
    File,
    Other,
}

/// 路径本身是什么（软链不跟随）；不存在是 `None`。
pub fn kind(path: &Path) -> Option<Kind> {
    let meta = fs::symlink_metadata(path).ok()?;
    let file_type = meta.file_type();
    Some(if file_type.is_symlink() {
        Kind::Symlink
    } else if file_type.is_dir() {
        Kind::Dir
    } else if file_type.is_file() {
        Kind::File
    } else {
        Kind::Other
    })
}

/// 解开软链后的真实路径；不存在或读不了时退回原路径。
pub fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// 目录里的条目，按名字排好。
pub fn entries(dir: &Path) -> io::Result<Vec<(String, Kind)>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = kind(&entry.path()).unwrap_or(Kind::Other);
        out.push((name, kind));
    }
    out.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(out)
}

/// 在 `at` 建一条指向 `target` 的软链。
pub fn symlink(target: &Path, at: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, at)
    }
    #[cfg(windows)]
    {
        if target.is_dir() {
            std::os::windows::fs::symlink_dir(target, at)
        } else {
            std::os::windows::fs::symlink_file(target, at)
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, at);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "symlinks are not supported",
        ))
    }
}

/// 只删软链本身，不碰它指着的东西。Windows 上指向目录的链接要按目录删。
pub fn remove_link(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        #[cfg(windows)]
        Err(_) => fs::remove_dir(path),
        #[cfg(not(windows))]
        Err(error) => Err(error),
    }
}

/// 整树复制。跳过软链：软链是别人的引用，不是技能的内容。
pub fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for (name, kind) in entries(from)? {
        let source = from.join(&name);
        let destination = to.join(&name);
        match kind {
            Kind::Dir => copy_tree(&source, &destination)?,
            Kind::File => {
                fs::copy(&source, &destination)?;
            }
            Kind::Symlink | Kind::Other => {}
        }
    }
    Ok(())
}

/// 交换两个都存在的目录。macOS、Linux 是原子的：已经指着 `right` 的链接全程不悬空。
pub fn exchange(left: &Path, right: &Path) -> io::Result<()> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let nul = |_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte");
        let left = CString::new(left.as_os_str().as_bytes()).map_err(nul)?;
        let right = CString::new(right.as_os_str().as_bytes()).map_err(nul)?;
        // SAFETY: 两个都是以 NUL 结尾、调用期间一直活着的 C 字符串。
        let rc = unsafe {
            #[cfg(target_os = "macos")]
            {
                libc::renamex_np(left.as_ptr(), right.as_ptr(), libc::RENAME_SWAP)
            }
            #[cfg(target_os = "linux")]
            {
                libc::renameat2(
                    libc::AT_FDCWD,
                    left.as_ptr(),
                    libc::AT_FDCWD,
                    right.as_ptr(),
                    libc::RENAME_EXCHANGE,
                )
            }
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let aside = right.with_extension("exchange");
        fs::rename(right, &aside)?;
        fs::rename(left, right)?;
        fs::rename(&aside, left)
    }
}

/// 把 `from` 移进 `root`，名字叫 `name`，重名时加序号。
pub fn move_into(root: &Path, from: &Path, name: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(root)?;
    let mut destination = root.join(name);
    let mut sequence = 1;
    while kind(&destination).is_some() {
        destination = root.join(format!("{name}-{sequence}"));
        sequence += 1;
    }
    fs::rename(from, &destination)?;
    Ok(destination)
}

/// 空目录就删掉；不是目录、不空、删不了都不管。
pub fn remove_empty_dir(path: &Path) {
    if kind(path) != Some(Kind::Dir) {
        return;
    }
    if entries(path).is_ok_and(|entries| entries.is_empty()) {
        let _ = fs::remove_dir(path);
    }
}

/// 文件名，取不到就是整个路径。
pub fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}
