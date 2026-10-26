//! 守护映像只写一次。`smeltd` 只是同目录里的启动名，是一条符号链接。
//!
//! macOS 没有 fexecve。正在映射的那个文件不能改名、不能截断，启动名也不能在
//! 进程活着的时候被重新解析成另一份映像。会话宿主必须 exec 启动时钉住的普通文件。
//! 升级只是再写一份新映像，提交成功后再把链接拨过去；回滚不动链接。
//! 没有被启动名或 `smeltd.pending` 指向、并且 `lsof` 证明没有进程映射的旧映像可以删。
//! 证明不了就留下。

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

pub const LAUNCH_NAME: &str = "smeltd";
pub const PENDING_NAME: &str = "smeltd.pending";
const IMAGE_PREFIX: &str = "smeltd-";

/// 守护映像的内容指纹：SHA-256、128 KiB 一块。映像文件名、进程启动时钉住的
/// 指纹、GUI 判断守护是否过期，都用这同一个值。
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
    if cfg!(windows) {
        format!("{IMAGE_PREFIX}{fingerprint}.exe")
    } else {
        format!("{IMAGE_PREFIX}{fingerprint}")
    }
}

pub fn is_immutable_image_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(IMAGE_PREFIX) else {
        return false;
    };
    let rest = rest.strip_suffix(".exe").unwrap_or(rest);
    rest.len() == 64
        && rest
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// 真正拿去执行的那份映像。Unix 是启动名那条符号链接；Windows 没有符号链接权限时，
/// 启动名是一份指针文件，这里返回它指向的 `smeltd-<指纹>.exe`。
pub fn launch_executable(dir: &Path) -> PathBuf {
    #[cfg(unix)]
    {
        dir.join(LAUNCH_NAME)
    }
    #[cfg(windows)]
    {
        read_image_pointer(dir, LAUNCH_NAME).unwrap_or_else(|| dir.join("smeltd.exe"))
    }
    #[cfg(not(any(unix, windows)))]
    {
        dir.join(LAUNCH_NAME)
    }
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
    let linked = place_image(&tmp, &dest);
    let _ = fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(dest),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            existing_image(&dest, &fingerprint)?.ok_or(error)
        }
        Err(error) => Err(error),
    }
}

/// 原子替换同目录里的启动名。Unix 是符号链接；Windows 是一份指针文件，
/// 内容是映像文件名。两边的目标都只写文件名，目录挪走仍然指向旁边的映像。
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
    let write_tmp = (|| {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&image_name, &tmp)
        }
        #[cfg(windows)]
        {
            fs::write(
                &tmp,
                format!("smelt-image-v1\n{}\n", image_name.to_string_lossy()),
            )
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = &image_name;
            Err(io::Error::new(io::ErrorKind::Unsupported, "不能发布启动名"))
        }
    })();
    if let Err(error) = write_tmp {
        let _ = fs::remove_file(&tmp);
        return Err(error);
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

/// `smeltd.pending` 解析后的普通文件。链接断了、指针指空了，都当没有。
pub fn pending_image(dir: &Path) -> Option<PathBuf> {
    #[cfg(unix)]
    {
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
    #[cfg(windows)]
    {
        read_image_pointer(dir, PENDING_NAME)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = dir;
        None
    }
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

fn place_image(tmp: &Path, dest: &Path) -> io::Result<()> {
    match fs::hard_link(tmp, dest) {
        Ok(()) => Ok(()),
        #[cfg(windows)]
        Err(error) if error.kind() != io::ErrorKind::AlreadyExists => {
            fs::copy(tmp, dest).map(|_| ())
        }
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn read_image_pointer(dir: &Path, name: &str) -> Option<PathBuf> {
    let text = fs::read_to_string(dir.join(name)).ok()?;
    let mut lines = text.lines();
    if lines.next()? != "smelt-image-v1" {
        return None;
    }
    let image_name = lines.next()?;
    if !is_immutable_image_name(image_name) {
        return None;
    }
    let path = dir.join(image_name);
    fs::metadata(&path)
        .ok()
        .filter(|meta| meta.is_file())
        .and_then(|_| fs::canonicalize(&path).ok())
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
    if !same_regular_file(image, &expected) {
        return Err(io::Error::other("映像和启动名必须在同一目录"));
    }
    Ok(name)
}

fn same_regular_file(left: &Path, right: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let (Ok(left), Ok(right)) = (fs::symlink_metadata(left), fs::symlink_metadata(right)) else {
            return false;
        };
        left.is_file() && right.is_file() && left.dev() == right.dev() && left.ino() == right.ino()
    }
    #[cfg(windows)]
    {
        let (Ok(left_meta), Ok(right_meta)) = (fs::metadata(left), fs::metadata(right)) else {
            return false;
        };
        if !left_meta.is_file() || !right_meta.is_file() {
            return false;
        }
        matches!(
            (fs::canonicalize(left), fs::canonicalize(right)),
            (Ok(left), Ok(right)) if left == right
        )
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (left, right);
        false
    }
}

/// `lsof` 的约定与在线更新一致：退出码 0 表示至少有一个打开项；
/// 退出码 1 且标准输出、标准错误都为空才是没人用。其余都不能证明安全。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MappedState {
    InUse,
    Unused,
    Unknown,
}

pub fn classify_lsof_probe(
    success: bool,
    code: Option<i32>,
    stdout: &[u8],
    stderr: &[u8],
) -> MappedState {
    if success {
        return MappedState::InUse;
    }
    if code == Some(1) && stdout.is_empty() && stderr.is_empty() {
        return MappedState::Unused;
    }
    MappedState::Unknown
}

/// 删掉没有被 `smeltd` / `smeltd.pending` 指向、并且探测证明没有进程映射的旧映像。
/// 删除某一份失败时仍继续尝试其余映像，最后返回第一个删除错误。
pub fn reap_unused_images(dir: &Path) -> io::Result<()> {
    reap_unused_images_with(dir, probe_image_mapped)
}

fn reap_unused_images_with(
    dir: &Path,
    mut probe: impl FnMut(&Path) -> MappedState,
) -> io::Result<()> {
    let keep = referenced_image_ids(dir)?;
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut first_delete_error = None;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !is_immutable_image_name(name) {
            continue;
        }
        let path = entry.path();
        match fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        }
        let Some(id) = file_id(&path) else {
            continue;
        };
        if keep.iter().any(|kept| kept == &id) {
            continue;
        }
        if probe(&path) != MappedState::Unused {
            continue;
        }
        if let Err(error) = fs::remove_file(&path)
            && error.kind() != io::ErrorKind::NotFound
            && first_delete_error.is_none()
        {
            first_delete_error = Some(error);
        }
    }
    match first_delete_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Linux：直接扫 `/proc/*/maps`，按设备号和 inode 找映射了这份文件的进程，不依赖
/// lsof（很多机器不装）。同一用户的进程读不到 maps 就证明不了，按未知处理；别的用户
/// 的进程碰不到 `~/.smelt` 里的映像，跳过。
#[cfg(target_os = "linux")]
pub(crate) fn probe_image_mapped(path: &Path) -> MappedState {
    use std::os::unix::fs::MetadataExt as _;

    let Ok(meta) = fs::metadata(path) else {
        return MappedState::Unknown;
    };
    let Ok(processes) = fs::read_dir("/proc") else {
        return MappedState::Unknown;
    };
    // SAFETY: getuid 无参数、不失败。
    let uid = unsafe { libc::getuid() };
    for process in processes.flatten() {
        if !process
            .file_name()
            .to_str()
            .is_some_and(|name| name.bytes().all(|byte| byte.is_ascii_digit()))
        {
            continue;
        }
        let maps = match fs::read_to_string(process.path().join("maps")) {
            Ok(maps) => maps,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => {
                let ours = fs::metadata(process.path()).is_ok_and(|meta| meta.uid() == uid);
                if ours {
                    return MappedState::Unknown;
                }
                continue;
            }
        };
        if maps_reference(&maps, meta.dev(), meta.ino()) {
            return MappedState::InUse;
        }
    }
    MappedState::Unused
}

/// `/proc/<pid>/maps` 一行：`地址 权限 偏移 主:次设备号(十六进制) inode 路径`。
#[cfg(any(target_os = "linux", test))]
fn maps_reference(maps: &str, dev: u64, ino: u64) -> bool {
    maps.lines().any(|line| {
        let mut fields = line.split_whitespace().skip(3);
        let (Some(device), Some(inode)) = (fields.next(), fields.next()) else {
            return false;
        };
        let Some((major, minor)) = device.split_once(':') else {
            return false;
        };
        let (Ok(major), Ok(minor), Ok(inode)) = (
            u32::from_str_radix(major, 16),
            u32::from_str_radix(minor, 16),
            inode.parse::<u64>(),
        ) else {
            return false;
        };
        inode == ino && linux_makedev(major, minor) == dev
    })
}

/// glibc `makedev` 的编码：高位的主次号拆开放，`stat` 的 `st_dev` 用的就是它。
#[cfg(any(target_os = "linux", test))]
fn linux_makedev(major: u32, minor: u32) -> u64 {
    let (major, minor) = (u64::from(major), u64::from(minor));
    ((major & 0xffff_f000) << 32)
        | ((major & 0x0000_0fff) << 8)
        | ((minor & 0xffff_ff00) << 12)
        | (minor & 0x0000_00ff)
}

#[cfg(all(unix, not(target_os = "linux")))]
pub(crate) fn probe_image_mapped(path: &Path) -> MappedState {
    let output = match std::process::Command::new("/usr/sbin/lsof")
        .arg("-Fn")
        .arg("--")
        .arg(path)
        .output()
    {
        Ok(output) => output,
        Err(_) => return MappedState::Unknown,
    };
    classify_lsof_probe(
        output.status.success(),
        output.status.code(),
        &output.stdout,
        &output.stderr,
    )
}

/// 正在映射的 exe 不能再以写方式打开。打得开就没人执行它。
#[cfg(windows)]
pub(crate) fn probe_image_mapped(path: &Path) -> MappedState {
    match OpenOptions::new().read(true).write(true).open(path) {
        Ok(_) => MappedState::Unused,
        Err(error) if error.raw_os_error() == Some(32) => MappedState::InUse,
        Err(error) if error.kind() == io::ErrorKind::NotFound => MappedState::Unused,
        Err(_) => MappedState::Unknown,
    }
}

#[derive(Clone, PartialEq, Eq)]
struct FileId {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(windows)]
    path: PathBuf,
}

fn file_id(path: &Path) -> Option<FileId> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = fs::symlink_metadata(path).ok()?;
        if !meta.file_type().is_file() {
            return None;
        }
        Some(FileId {
            dev: meta.dev(),
            ino: meta.ino(),
        })
    }
    #[cfg(windows)]
    {
        let meta = fs::metadata(path).ok()?;
        if !meta.is_file() {
            return None;
        }
        Some(FileId {
            path: fs::canonicalize(path).ok()?,
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        None
    }
}

fn referenced_image_ids(dir: &Path) -> io::Result<Vec<FileId>> {
    let mut ids = Vec::new();
    for name in [LAUNCH_NAME, PENDING_NAME] {
        if let Some(id) = resolved_image_id(dir, name)? {
            ids.push(id);
        }
    }
    Ok(ids)
}

fn resolved_image_id(dir: &Path, name: &str) -> io::Result<Option<FileId>> {
    let resolved = {
        #[cfg(unix)]
        {
            let link = dir.join(name);
            let meta = match fs::symlink_metadata(&link) {
                Ok(meta) => meta,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error),
            };
            if !meta.file_type().is_symlink() {
                return Ok(None);
            }
            match fs::canonicalize(&link) {
                Ok(resolved) => resolved,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error),
            }
        }
        #[cfg(windows)]
        {
            let Some(resolved) = read_image_pointer(dir, name) else {
                return Ok(None);
            };
            resolved
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (dir, name);
            return Ok(None);
        }
    };
    let Some(id) = file_id(&resolved) else {
        return Ok(None);
    };
    let Some(parent) = resolved.parent() else {
        return Ok(None);
    };
    let dir = fs::canonicalize(dir)?;
    let parent = fs::canonicalize(parent)?;
    if parent != dir {
        return Ok(None);
    }
    Ok(Some(id))
}

fn remove_pending_symlink(dir: &Path) -> io::Result<()> {
    let path = dir.join(PENDING_NAME);
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_symlink() || (cfg!(windows) && meta.is_file()) => {
            fs::remove_file(path)
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
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
    fn publishing_the_same_bytes_keeps_the_same_inode() {
        let dir = scratch("republish");
        let src = dir.join("src");
        fs::write(&src, b"same-image").unwrap();

        let first = publish_image(&dir, &src).unwrap();
        #[cfg(unix)]
        let inode = fs::metadata(&first).unwrap().ino();
        let second = publish_image(&dir, &src).unwrap();

        assert_eq!(first, second);
        #[cfg(unix)]
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
        #[cfg(unix)]
        let inode = fs::metadata(&image).unwrap().ino();

        let error = publish_image(&dir, &src).expect_err("内容对不上时必须拒绝覆盖");
        assert!(error.to_string().contains("拒绝覆盖"), "实际错误：{error}");
        assert_eq!(fs::read(&image).unwrap(), b"not-the-same-bytes");
        #[cfg(unix)]
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
        #[cfg(unix)]
        let old_inode = fs::metadata(&old_image).unwrap().ino();

        retarget_launch_symlink(&dir, &old_image).unwrap();
        retarget_launch_symlink(&dir, &new_image).unwrap();

        #[cfg(unix)]
        {
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
        }
        #[cfg(windows)]
        assert_eq!(
            launch_executable(&dir),
            fs::canonicalize(&new_image).unwrap()
        );
        assert_eq!(fs::read(&old_image).unwrap(), b"image-old");
        #[cfg(unix)]
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

    #[test]
    fn lsof_success_means_mapped_and_only_empty_exit_one_means_unused() {
        assert_eq!(
            classify_lsof_probe(true, Some(0), b"p1\n", b""),
            MappedState::InUse
        );
        assert_eq!(
            classify_lsof_probe(false, Some(1), b"", b""),
            MappedState::Unused
        );
        assert_eq!(
            classify_lsof_probe(false, Some(1), b"", b"warn"),
            MappedState::Unknown
        );
        assert_eq!(
            classify_lsof_probe(false, Some(2), b"", b""),
            MappedState::Unknown
        );
        assert_eq!(
            classify_lsof_probe(false, None, b"", b""),
            MappedState::Unknown
        );
    }

    #[test]
    fn proc_maps_match_on_device_and_inode() {
        let maps = "\
55d0c0a00000-55d0c0a20000 r--p 00000000 fd:01 1048602 /root/.smelt/bin/smeltd-ab
7f1c2e000000-7f1c2e021000 rw-p 00000000 00:00 0
";
        assert!(maps_reference(maps, linux_makedev(0xfd, 1), 1048602));
        assert!(!maps_reference(maps, linux_makedev(0xfd, 1), 1048603));
        assert!(!maps_reference(maps, linux_makedev(8, 1), 1048602));
        assert_eq!(linux_makedev(8, 1), 0x801);
    }

    /// 自己正在执行的程序一定被映射着；刚写出来没人执行的文件一定没有。
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_probe_sees_the_running_test_binary() {
        let exe = std::env::current_exe().unwrap();
        assert_eq!(probe_image_mapped(&exe), MappedState::InUse);
        let dir = scratch("probe");
        let idle = dir.join("idle");
        fs::write(&idle, b"idle").unwrap();
        assert_eq!(probe_image_mapped(&idle), MappedState::Unused);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reap_keeps_launch_pending_and_anything_not_proven_unused() {
        let dir = scratch("reap");
        let launch_src = dir.join("launch-src");
        let pending_src = dir.join("pending-src");
        let mapped_src = dir.join("mapped-src");
        let unknown_src = dir.join("unknown-src");
        let stale_src = dir.join("stale-src");
        fs::write(&launch_src, b"launch").unwrap();
        fs::write(&pending_src, b"pending").unwrap();
        fs::write(&mapped_src, b"mapped").unwrap();
        fs::write(&unknown_src, b"unknown").unwrap();
        fs::write(&stale_src, b"stale").unwrap();
        let launch = publish_image(&dir, &launch_src).unwrap();
        let pending = publish_image(&dir, &pending_src).unwrap();
        let mapped = publish_image(&dir, &mapped_src).unwrap();
        let unknown = publish_image(&dir, &unknown_src).unwrap();
        let stale = publish_image(&dir, &stale_src).unwrap();
        retarget_launch_symlink(&dir, &launch).unwrap();
        retarget_pending_symlink(&dir, &pending).unwrap();

        reap_unused_images_with(&dir, |path| {
            if path == mapped {
                MappedState::InUse
            } else if path == unknown {
                MappedState::Unknown
            } else {
                MappedState::Unused
            }
        })
        .unwrap();

        assert!(launch.is_file(), "启动名指向的映像必须留下");
        assert!(pending.is_file(), "pending 指向的映像必须留下");
        assert!(mapped.is_file(), "仍被映射的映像必须留下");
        assert!(unknown.is_file(), "无法证明空闲的映像必须留下");
        assert!(!stale.exists(), "既没人指向也没人映射的旧映像要删掉");
        fs::remove_dir_all(dir).unwrap();
    }
}
