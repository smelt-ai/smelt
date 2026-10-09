//! 守护映像只写一次。`smeltd` 只是同目录里的启动名，是一条符号链接。
//!
//! macOS 没有 fexecve。正在映射的那个文件不能改名、不能截断，启动名也不能在
//! 进程活着的时候被重新解析成另一份映像。会话宿主必须 exec 启动时钉住的普通文件。
//! 升级只是再写一份新映像，提交成功后再把链接拨过去；回滚不动链接。

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

pub const LAUNCH_NAME: &str = "smeltd";
pub const PENDING_NAME: &str = "smeltd.pending";
const IMAGE_PREFIX: &str = "smeltd-";

/// 与 `smelt_plugin_host::executable_fingerprint` 同一算法：SHA-256、128 KiB 一块。
/// 文件名里的指纹必须和插件宿主算出来的一致，否则待升级映像会对不上。
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
    Ok(format!("{:x}", digest.finalize()))
}

pub fn image_file_name(fingerprint: &str) -> String {
    format!("{IMAGE_PREFIX}{fingerprint}")
}

pub fn is_immutable_image_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(IMAGE_PREFIX) else {
        return false;
    };
    rest.len() == 64
        && rest
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// 把 `src` 的字节写成 `dir/smeltd-<指纹>`。同内容再发布一次必须还是同一个 inode。
/// 名字已经存在但内容对不上时直接报错，绝不覆盖。
pub fn publish_image(dir: &Path, src: &Path) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let fingerprint = content_fingerprint(src)?;
    let name = image_file_name(&fingerprint);
    let dest = dir.join(&name);
    if let Some(existing) = existing_image(&dest, &fingerprint)? {
        return Ok(existing);
    }
    let tmp = dir.join(format!(
        ".{name}.partial-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let copied = write_temp(&tmp, src);
    if let Err(error) = copied {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    let linked = fs::hard_link(&tmp, &dest);
    let _ = fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(dest),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            existing_image(&dest, &fingerprint)?.ok_or(error)
        }
        Err(error) => Err(error),
    }
}

/// 原子替换同目录里的一条符号链接。目标只写文件名，目录挪走链接仍然指向旁边的映像。
pub fn retarget_symlink(dir: &Path, link_name: &str, image: &Path) -> io::Result<()> {
    if is_immutable_image_name(link_name) {
        return Err(io::Error::other(format!("不能改写映像文件：{link_name}")));
    }
    let image_name = image_name_in_dir(dir, image)?;
    let tmp = dir.join(format!(
        ".{link_name}.link-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&image_name, &tmp)?;
    }
    #[cfg(not(unix))]
    {
        let _ = image_name;
        return Err(unsupported());
    }
    if let Err(error) = fs::rename(&tmp, dir.join(link_name)) {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    Ok(())
}

pub fn retarget_launch_symlink(dir: &Path, image: &Path) -> io::Result<()> {
    retarget_symlink(dir, LAUNCH_NAME, image)
}

pub fn retarget_pending_symlink(dir: &Path, image: &Path) -> io::Result<()> {
    retarget_symlink(dir, PENDING_NAME, image)
}

/// `smeltd.pending` 解析后的普通文件。链接断了、或这个名字本身是普通文件，都当没有。
pub fn pending_image(dir: &Path) -> Option<PathBuf> {
    let link = dir.join(PENDING_NAME);
    let meta = fs::symlink_metadata(&link).ok()?;
    if !meta.file_type().is_symlink() {
        return None;
    }
    let resolved = fs::canonicalize(&link).ok()?;
    fs::symlink_metadata(&resolved)
        .ok()
        .filter(|meta| meta.is_file())
        .map(|_| resolved)
}

/// 交接提交之后把启动名拨到这次 exec 的映像，并拆掉 pending 链接。
/// 不删除 exec 的那个文件：正在跑的进程还映射着它。
pub fn retarget_launch_after_commit(executed: &Path) -> io::Result<PathBuf> {
    let dir = executed
        .parent()
        .ok_or_else(|| io::Error::other(format!("可执行文件没有目录：{}", executed.display())))?;
    let name = executed
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other(format!("可执行文件没有文件名：{}", executed.display())))?;
    let image = if is_immutable_image_name(name) {
        let meta = fs::symlink_metadata(executed)?;
        if !meta.is_file() {
            return Err(io::Error::other(format!(
                "映像不是普通文件：{}",
                executed.display()
            )));
        }
        executed.to_path_buf()
    } else {
        publish_image(dir, executed)?
    };
    retarget_launch_symlink(dir, &image)?;
    remove_pending_symlink(dir)?;
    Ok(image)
}

fn existing_image(dest: &Path, fingerprint: &str) -> io::Result<Option<PathBuf>> {
    match fs::symlink_metadata(dest) {
        Ok(meta) if meta.is_file() => {
            let existing = content_fingerprint(dest)?;
            if existing == fingerprint {
                Ok(Some(dest.to_path_buf()))
            } else {
                Err(io::Error::other(format!(
                    "映像名与内容不一致，拒绝覆盖：{}",
                    dest.display()
                )))
            }
        }
        Ok(_) => Err(io::Error::other(format!(
            "映像路径不是普通文件，拒绝覆盖：{}",
            dest.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
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
        let mut perms = output.metadata()?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(tmp, perms)?;
    }
    Ok(())
}

fn image_name_in_dir(dir: &Path, image: &Path) -> io::Result<std::ffi::OsString> {
    let name = image
        .file_name()
        .ok_or_else(|| io::Error::other(format!("映像没有文件名：{}", image.display())))?
        .to_os_string();
    let name_str = name
        .to_str()
        .ok_or_else(|| io::Error::other(format!("映像文件名不是文本：{}", image.display())))?;
    if !is_immutable_image_name(name_str) {
        return Err(io::Error::other(format!(
            "只能把链接指向 {IMAGE_PREFIX}<指纹>：{name_str}"
        )));
    }
    let expected = dir.join(&name);
    let image_meta = fs::symlink_metadata(image)?;
    let expected_meta = fs::symlink_metadata(&expected)
        .map_err(|_| io::Error::other("映像和启动名必须在同一目录"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if !image_meta.is_file()
            || !expected_meta.is_file()
            || image_meta.dev() != expected_meta.dev()
            || image_meta.ino() != expected_meta.ino()
        {
            return Err(io::Error::other("映像和启动名必须在同一目录"));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (image_meta, expected_meta);
        return Err(unsupported());
    }
    Ok(name)
}

fn remove_pending_symlink(dir: &Path) -> io::Result<()> {
    let path = dir.join(PENDING_NAME);
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_symlink() => fs::remove_file(path),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(not(unix))]
fn unsupported() -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, "守护映像只支持 Unix 符号链接")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
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
    fn publishing_the_same_bytes_keeps_the_same_inode() {
        let dir = scratch("republish");
        let src = dir.join("src");
        fs::write(&src, b"same-image").unwrap();

        let first = publish_image(&dir, &src).unwrap();
        let inode = fs::metadata(&first).unwrap().ino();
        let second = publish_image(&dir, &src).unwrap();

        assert_eq!(first, second);
        assert_eq!(fs::metadata(&second).unwrap().ino(), inode);
        assert!(is_immutable_image_name(
            first.file_name().unwrap().to_str().unwrap()
        ));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_name_whose_bytes_do_not_match_is_not_overwritten() {
        let dir = scratch("mismatch");
        let src = dir.join("src");
        fs::write(&src, b"alpha-image").unwrap();
        let image = publish_image(&dir, &src).unwrap();
        fs::remove_file(&image).unwrap();
        fs::write(&image, b"not-the-same-bytes").unwrap();
        let inode = fs::metadata(&image).unwrap().ino();

        let error = publish_image(&dir, &src).expect_err("内容对不上时必须拒绝覆盖");
        assert!(error.to_string().contains("拒绝覆盖"), "实际错误：{error}");
        assert_eq!(fs::read(&image).unwrap(), b"not-the-same-bytes");
        assert_eq!(fs::metadata(&image).unwrap().ino(), inode);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn flipping_the_launch_name_does_not_touch_the_image() {
        let dir = scratch("flip");
        let old_src = dir.join("old");
        let new_src = dir.join("new");
        fs::write(&old_src, b"image-old").unwrap();
        fs::write(&new_src, b"image-new").unwrap();
        let old_image = publish_image(&dir, &old_src).unwrap();
        let new_image = publish_image(&dir, &new_src).unwrap();
        let old_inode = fs::metadata(&old_image).unwrap().ino();

        retarget_launch_symlink(&dir, &old_image).unwrap();
        retarget_launch_symlink(&dir, &new_image).unwrap();

        let launch = dir.join(LAUNCH_NAME);
        assert_eq!(
            fs::read_link(&launch).unwrap(),
            new_image.file_name().unwrap()
        );
        assert!(
            fs::symlink_metadata(&launch)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&old_image).unwrap(), b"image-old");
        assert_eq!(fs::metadata(&old_image).unwrap().ino(), old_inode);
        assert_eq!(fs::read(&new_image).unwrap(), b"image-new");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pending_symlink_resolves_to_the_image() {
        let dir = scratch("pending");
        let src = dir.join("src");
        fs::write(&src, b"pending-image").unwrap();
        let image = publish_image(&dir, &src).unwrap();
        retarget_pending_symlink(&dir, &image).unwrap();

        assert_eq!(
            pending_image(&dir).unwrap(),
            fs::canonicalize(&image).unwrap()
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_link_cannot_point_at_an_image_in_another_directory() {
        let dir = scratch("home");
        let other = scratch("other");
        let src = other.join("src");
        fs::write(&src, b"elsewhere").unwrap();
        let image = publish_image(&other, &src).unwrap();

        let error = retarget_launch_symlink(&dir, &image).expect_err("不能链到别的目录");
        assert!(error.to_string().contains("同一目录"), "实际错误：{error}");
        fs::remove_dir_all(dir).unwrap();
        fs::remove_dir_all(other).unwrap();
    }
}
