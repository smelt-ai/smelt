//! 装在 `~/.smelt/bin` 里的那份 smeltd。名字固定：Unix 是 `smeltd`，Windows 是 `smeltd.exe`。
//!
//! 守护不从 App 包里跑：拖 DMG 覆盖 App 时内核会杀掉映射着被覆盖文件的进程。换版本时守护
//! 已经停了（桌面和更新流程都是先停再装，见 `smelt::local_daemon`）；Linux 运行时自己升级时
//! 还在跑，靠的是先写到旁边再 rename 盖过去，正在跑的进程握着的是原来那个文件，不受影响。

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// 启动名，也是 Unix 上的文件名。
pub const LAUNCH_NAME: &str = "smeltd";

pub fn file_name() -> &'static str {
    if cfg!(windows) {
        "smeltd.exe"
    } else {
        LAUNCH_NAME
    }
}

/// `dir` 里装着的那份 smeltd 的路径（不管装没装）。
pub fn installed_path(dir: &Path) -> PathBuf {
    dir.join(file_name())
}

/// 内容指纹：SHA-256、128 KiB 一块。进程启动时钉住的指纹、桌面判断守护是否过期、
/// 下载包的校验都用它。
pub fn content_fingerprint(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

/// 把 `src` 装成 `dir` 里的 smeltd：先在同目录写一份临时文件、落盘，再 rename 盖过去。
/// 装着的已经是同样内容的普通文件时不动它（新文件在 macOS 上第一次执行要重新验证），返回
/// `false`；换了返回 `true`。
///
/// Windows 上目标正被执行时 rename 会失败，原样报错：调用方应当先停掉守护。
pub fn install(dir: &Path, src: &Path) -> io::Result<bool> {
    fs::create_dir_all(dir)?;
    let dest = installed_path(dir);
    if fs::symlink_metadata(&dest).is_ok_and(|meta| meta.is_file()) && same_contents(src, &dest)? {
        return Ok(false);
    }
    let tmp = dir.join(format!(
        ".{LAUNCH_NAME}.new-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let result = write_temp(&tmp, src).and_then(|()| fs::rename(&tmp, &dest));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map(|()| true)
}

/// 两个文件内容是否一样。先比长度，再逐块比。
fn same_contents(a: &Path, b: &Path) -> io::Result<bool> {
    if fs::metadata(a)?.len() != fs::metadata(b)?.len() {
        return Ok(false);
    }
    let (mut a, mut b) = (File::open(a)?, File::open(b)?);
    let mut left = vec![0_u8; 128 * 1024];
    let mut right = vec![0_u8; 128 * 1024];
    loop {
        let read = a.read(&mut left)?;
        if read == 0 {
            return Ok(true);
        }
        b.read_exact(&mut right[..read])?;
        if left[..read] != right[..read] {
            return Ok(false);
        }
    }
}

fn write_temp(tmp: &Path, src: &Path) -> io::Result<()> {
    let mut input = File::open(src)?;
    let mut output = OpenOptions::new().write(true).create_new(true).open(tmp)?;
    io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(tmp, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "smelt-daemon-image-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn install_replaces_the_file_under_the_fixed_name() {
        let dir = scratch("replace");
        let (old, new) = (dir.join("old"), dir.join("new"));
        fs::write(&old, b"image-old").unwrap();
        fs::write(&new, b"image-new").unwrap();

        assert!(install(&dir, &old).unwrap());
        let installed = installed_path(&dir);
        // 模拟正在跑的旧进程握着这个文件。
        let held = File::open(&installed).unwrap();
        assert!(install(&dir, &new).unwrap());

        assert_eq!(fs::read(&installed).unwrap(), b"image-new");
        #[cfg(unix)]
        {
            assert_ne!(
                held.metadata().unwrap().ino(),
                fs::metadata(&installed).unwrap().ino(),
                "新版本必须是新文件，旧进程握着的那份不能被原地改写"
            );
            use std::os::unix::fs::PermissionsExt as _;
            assert_ne!(
                fs::metadata(&installed).unwrap().permissions().mode() & 0o111,
                0
            );
        }
        drop(held);
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with('.'))
            .collect();
        assert!(leftovers.is_empty(), "临时文件要么改名成功要么删掉");
        fs::remove_dir_all(dir).unwrap();
    }

    /// 内容没变不重写：装着的文件还是原来那个。
    #[test]
    fn installing_the_same_contents_keeps_the_file() {
        let dir = scratch("same");
        let (src, copy) = (dir.join("src"), dir.join("copy"));
        fs::write(&src, b"image").unwrap();
        fs::write(&copy, b"image").unwrap();
        assert!(install(&dir, &src).unwrap());
        #[cfg(unix)]
        let ino = fs::metadata(installed_path(&dir)).unwrap().ino();
        assert!(!install(&dir, &copy).unwrap());
        assert!(
            !install(&dir, &installed_path(&dir)).unwrap(),
            "装自己也不动"
        );
        #[cfg(unix)]
        assert_eq!(fs::metadata(installed_path(&dir)).unwrap().ino(), ino);
        fs::write(&copy, b"imagf").unwrap();
        assert!(install(&dir, &copy).unwrap(), "同样长度、内容不同要换");
        assert_eq!(fs::read(installed_path(&dir)).unwrap(), b"imagf");
        fs::remove_dir_all(dir).unwrap();
    }

    /// 旧布局里启动名是指向 `smeltd-<指纹>` 的链接：装的时候把链接本身换成文件。
    #[cfg(unix)]
    #[test]
    fn install_turns_a_launch_symlink_into_a_regular_file() {
        let dir = scratch("symlink");
        let image = dir.join("image");
        fs::write(&image, b"image-old").unwrap();
        std::os::unix::fs::symlink("image", dir.join(LAUNCH_NAME)).unwrap();
        let src = dir.join("src");
        fs::write(&src, b"image-new").unwrap();

        assert!(install(&dir, &src).unwrap());

        let meta = fs::symlink_metadata(dir.join(LAUNCH_NAME)).unwrap();
        assert!(meta.file_type().is_file());
        assert_eq!(fs::read(&image).unwrap(), b"image-old");
        fs::remove_dir_all(dir).unwrap();
    }
}
