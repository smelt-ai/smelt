//! 编译好的 smelt-pi 发布在 `~/.smelt/bin/smelt-pi-<指纹>/`，同目录的 `smelt-pi` 是指向
//! 当前那份可执行文件的符号链接。
//!
//! 指纹按 smelt-pi、photon wasm、`native/` 和内置技能 `skills/` 这一整套算，和守护映像无关：只改了 Pi
//! 也能换上新的一份，不用等守护交接。目录发布后不再改动。守护每次起 Pi 都解析
//! 链接、exec 解析出来的真实路径，所以拨链接不影响已经在跑的 Pi；旧目录等里面的
//! smelt-pi 没人映射了再删。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::daemon_image::{MappedState, content_fingerprint, probe_image_mapped};

/// 启动链接名。目录里的可执行文件在 Windows 上是 `smelt-pi.exe`，见 [`binary_file_name`]。
pub const LAUNCH_NAME: &str = "smelt-pi";

pub fn binary_file_name() -> &'static str {
    if cfg!(windows) {
        "smelt-pi.exe"
    } else {
        LAUNCH_NAME
    }
}
const WASM_NAME: &str = "photon_rs_bg.wasm";
const NATIVE_DIR: &str = "native";
/// 内置技能目录。只在智能体勾选时用 `--skill` 传给 Pi，不在 Pi 的自动发现路径上。
pub const SKILLS_DIR: &str = "skills";
const DIR_PREFIX: &str = "smelt-pi-";
/// App 里这一整套相对 `Contents` 的位置。wasm 不是代码，不能放进只收签名代码的 `Contents/MacOS`。
pub const APP_BUNDLE_DIR: &str = "Resources/smelt-pi";

/// 把 `src_dir`（App 的 `Contents/Resources/smelt-pi` 或 Linux 包目录）里的这一整套发布到 `dir`，
/// 再把 `dir/smelt-pi` 拨过去。同一套内容只发布一次。返回发布出的 smelt-pi。
pub fn publish(dir: &Path, src_dir: &Path) -> io::Result<PathBuf> {
    let files = bundle_files(src_dir)?;
    let fingerprint = bundle_fingerprint(src_dir, &files)?;
    let name = format!("{DIR_PREFIX}{fingerprint}");
    let target = dir.join(&name);
    if fs::symlink_metadata(&target).is_err() {
        publish_tree(dir, src_dir, &files, &name)?;
    }
    retarget(dir, &name)?;
    Ok(target.join(binary_file_name()))
}

/// `dir/smelt-pi` 现在指向的、能执行的那份，解析成真实路径。链接缺了、指空了都是 `None`。
/// 拿真实路径去 exec：之后链接拨到新版本，已经起来的 Pi 还在原来的目录里。
pub fn current(dir: &Path) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let executable = fs::canonicalize(dir.join(LAUNCH_NAME)).ok()?;
        let meta = fs::metadata(&executable).ok()?;
        (meta.is_file() && meta.permissions().mode() & 0o111 != 0).then_some(executable)
    }
    #[cfg(windows)]
    {
        read_pi_pointer(dir)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = dir;
        None
    }
}

/// `src_dir` 里有没有一整套可以发布的 smelt-pi（不跟符号链接）。
pub fn bundled(src_dir: &Path) -> bool {
    fs::symlink_metadata(src_dir.join(binary_file_name())).is_ok_and(|meta| meta.is_file())
}

/// 删掉启动链接没指向、里面的 smelt-pi 也证明没人映射的旧目录。证明不了就留下。
pub fn reap_unused(dir: &Path) -> io::Result<()> {
    reap_unused_with(dir, probe_image_mapped)
}

fn reap_unused_with(dir: &Path, mut probe: impl FnMut(&Path) -> MappedState) -> io::Result<()> {
    let current = current_dir_name(dir);
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut first_error = None;
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !is_published_dir_name(&name) || current.as_deref() == Some(name.as_str()) {
            continue;
        }
        let path = entry.path();
        if !fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_dir()) {
            continue;
        }
        let binary = path.join(binary_file_name());
        if fs::symlink_metadata(&binary).is_ok_and(|meta| meta.is_file())
            && probe(&binary) != MappedState::Unused
        {
            continue;
        }
        if let Err(error) = fs::remove_dir_all(&path)
            && error.kind() != io::ErrorKind::NotFound
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn is_published_dir_name(name: &str) -> bool {
    name.strip_prefix(DIR_PREFIX).is_some_and(|rest| {
        rest.len() == 64
            && rest
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// 这一套里的文件，相对 `src_dir`、排好序。smelt-pi 和 wasm 必须有，`native/` 可选。
/// 符号链接一律不跟。
fn bundle_files(src_dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for required in [binary_file_name(), WASM_NAME] {
        if !fs::symlink_metadata(src_dir.join(required)).is_ok_and(|meta| meta.is_file()) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("缺少 {}", src_dir.join(required).display()),
            ));
        }
        files.push(PathBuf::from(required));
    }
    for optional in [NATIVE_DIR, SKILLS_DIR] {
        let dir = src_dir.join(optional);
        if fs::symlink_metadata(&dir).is_ok_and(|meta| meta.is_dir()) {
            collect_regular_files(src_dir, &dir, &mut files)?;
        }
    }
    files.sort();
    Ok(files)
}

fn collect_regular_files(root: &Path, dir: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            collect_regular_files(root, &path, files)?;
        } else if meta.is_file() {
            files.push(path.strip_prefix(root).map_err(io::Error::other)?.to_path_buf());
        }
    }
    Ok(())
}

fn bundle_fingerprint(src_dir: &Path, files: &[PathBuf]) -> io::Result<String> {
    let mut digest = Sha256::new();
    for relative in files {
        digest.update(relative.to_string_lossy().as_bytes());
        digest.update([0]);
        digest.update(content_fingerprint(&src_dir.join(relative))?.as_bytes());
        digest.update(*b"\n");
    }
    Ok(format!("{:x}", digest.finalize()))
}

/// 先在同目录的私有临时目录里写齐、落盘，再一次 rename 出去。别人抢先发布了同一套
/// 也算成功。
fn publish_tree(dir: &Path, src_dir: &Path, files: &[PathBuf], name: &str) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let staging = dir.join(format!(
        ".{name}.partial-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let result = (|| {
        for relative in files {
            let dest = staging.join(relative);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(src_dir.join(relative), &dest)?;
            // Windows 的 FlushFileBuffers 要求写权限。只读打开会返回拒绝访问。
            #[cfg(windows)]
            {
                fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&dest)?
                    .sync_all()?;
            }
            #[cfg(not(windows))]
            {
                fs::File::open(&dest)?.sync_all()?;
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(
                staging.join(binary_file_name()),
                fs::Permissions::from_mode(0o755),
            )?;
        }
        fs::rename(&staging, dir.join(name))
    })();
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&staging);
        if fs::symlink_metadata(dir.join(name)).is_ok_and(|meta| meta.is_dir()) {
            return Ok(());
        }
        return Err(error);
    }
    Ok(())
}

/// 原子地把 `dir/smelt-pi` 指向 `<name>/smelt-pi`。链接只写相对路径。
fn retarget(dir: &Path, name: &str) -> io::Result<()> {
    let relative = format!("{name}/{}", binary_file_name());
    #[cfg(unix)]
    let link = dir.join(LAUNCH_NAME);
    #[cfg(windows)]
    let link = dir.join("smelt-pi.current");
    #[cfg(not(any(unix, windows)))]
    let link = dir.join(LAUNCH_NAME);
    #[cfg(unix)]
    if fs::read_link(&link).is_ok_and(|current| current == Path::new(&relative)) {
        return Ok(());
    }
    #[cfg(windows)]
    if read_pi_pointer(dir).is_some_and(|path| path.ends_with(Path::new(&relative))) {
        return Ok(());
    }
    let tmp = dir.join(format!(
        ".{LAUNCH_NAME}.link-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let written = (|| {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(relative, &tmp)
        }
        #[cfg(windows)]
        {
            fs::write(&tmp, format!("smelt-image-v1\n{relative}\n"))
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = &relative;
            Err(io::Error::new(io::ErrorKind::Unsupported, "不能发布 smelt-pi"))
        }
    })();
    if let Err(error) = written {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    if let Err(error) = fs::rename(&tmp, &link) {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    Ok(())
}

fn current_dir_name(dir: &Path) -> Option<String> {
    #[cfg(unix)]
    {
        fs::read_link(dir.join(LAUNCH_NAME))
            .ok()
            .and_then(|target| {
                target
                    .components()
                    .next()
                    .and_then(|first| first.as_os_str().to_str().map(str::to_string))
            })
    }
    #[cfg(windows)]
    {
        let text = fs::read_to_string(dir.join("smelt-pi.current")).ok()?;
        let mut lines = text.lines();
        if lines.next()? != "smelt-image-v1" {
            return None;
        }
        let rel = lines.next()?;
        rel.split(['/', '\\']).next().map(str::to_string)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = dir;
        None
    }
}

#[cfg(windows)]
fn read_pi_pointer(dir: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(dir.join("smelt-pi.current")).ok()?;
    let mut lines = text.lines();
    if lines.next()? != "smelt-image-v1" {
        return None;
    }
    let rel = lines.next()?;
    let mut path = dir.to_path_buf();
    for part in rel.split(['/', '\\']) {
        if part.is_empty() || part == "." {
            continue;
        }
        path.push(part);
    }
    let path = fs::canonicalize(path).ok()?;
    fs::metadata(&path)
        .ok()
        .filter(|meta| meta.is_file())
        .map(|_| path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "smelt-pi-image-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn package(dir: &Path, pi: &[u8], wasm: &[u8], native: Option<&[u8]>) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(binary_file_name()), pi).unwrap();
        fs::write(dir.join(WASM_NAME), wasm).unwrap();
        if let Some(native) = native {
            fs::create_dir_all(dir.join("native/sub")).unwrap();
            fs::write(dir.join("native/sub/addon.node"), native).unwrap();
        }
    }

    /// 只改了内置技能也是新的一套：智能体勾选时传的是当前链接后面那份。
    #[test]
    fn a_skill_only_change_publishes_a_new_dir() {
        let root = scratch("skills");
        let (pkg, bin) = (root.join("pkg"), root.join("bin"));
        package(&pkg, b"pi-v1", b"wasm", None);
        fs::create_dir_all(pkg.join("skills/smelt")).unwrap();
        fs::write(pkg.join("skills/smelt/SKILL.md"), "v1").unwrap();
        let first = publish(&bin, &pkg).unwrap();
        let first_dir = first.parent().unwrap();
        assert_eq!(fs::read_to_string(first_dir.join("skills/smelt/SKILL.md")).unwrap(), "v1");

        fs::write(pkg.join("skills/smelt/SKILL.md"), "v2").unwrap();
        let second = publish(&bin, &pkg).unwrap();
        assert_ne!(second, first);
        let second_dir = second.parent().unwrap();
        assert_eq!(fs::read_to_string(second_dir.join("skills/smelt/SKILL.md")).unwrap(), "v2");
        fs::remove_dir_all(root).unwrap();
    }

    /// 复现：守护字节没变、只换了 Pi，旧做法按守护指纹配对，新 Pi 发布不上去。
    #[test]
    fn a_pi_only_change_publishes_a_new_dir_and_moves_the_link() {
        let root = scratch("pi-only");
        let (pkg, bin) = (root.join("pkg"), root.join("bin"));
        package(&pkg, b"pi-v1", b"wasm", Some(b"native-v1"));
        let first = publish(&bin, &pkg).unwrap();
        assert_eq!(current(&bin), Some(fs::canonicalize(&first).unwrap()));
        let first_dir = first.parent().unwrap().to_path_buf();
        assert_eq!(fs::read(first_dir.join("native/sub/addon.node")).unwrap(), b"native-v1");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(&first).unwrap().permissions().mode();
            assert_ne!(mode & 0o111, 0, "smelt-pi 必须可执行");
        }

        assert_eq!(publish(&bin, &pkg).unwrap(), first, "同一套内容只发布一次");

        // 只改 native：指纹也要变。
        package(&pkg, b"pi-v1", b"wasm", Some(b"native-v2"));
        let second = publish(&bin, &pkg).unwrap();
        assert_ne!(second, first);
        assert_eq!(current(&bin), Some(fs::canonicalize(&second).unwrap()));
        assert_eq!(fs::read(&first).unwrap(), b"pi-v1", "旧目录原样留着给在跑的 Pi");
        fs::remove_dir_all(root).unwrap();
    }

    /// 守护起 Pi、Linux 运行时启动时补齐，都看这一个答案：链接缺了、指空了都算没有。
    #[test]
    fn current_is_the_real_path_behind_a_live_link() {
        let root = scratch("current");
        let (pkg, bin) = (root.join("pkg"), root.join("bin"));
        fs::create_dir_all(&bin).unwrap();
        assert!(current(&bin).is_none(), "没发布过");
        assert!(!bundled(&pkg));
        package(&pkg, b"pi-v1", b"wasm", None);
        assert!(bundled(&pkg));
        let published = publish(&bin, &pkg).unwrap();
        assert_eq!(current(&bin), Some(fs::canonicalize(&published).unwrap()));
        assert!(!bundled(&bin), "bin 里的 smelt-pi 是链接，不能当成一套包再发布");
        fs::remove_dir_all(published.parent().unwrap()).unwrap();
        assert!(current(&bin).is_none(), "链接指空了");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_package_without_wasm_is_refused_and_the_link_stays() {
        let root = scratch("no-wasm");
        let (pkg, bin) = (root.join("pkg"), root.join("bin"));
        package(&pkg, b"pi-v1", b"wasm", None);
        let first = publish(&bin, &pkg).unwrap();
        fs::write(pkg.join(binary_file_name()), b"pi-v2").unwrap();
        fs::remove_file(pkg.join(WASM_NAME)).unwrap();
        assert!(publish(&bin, &pkg).is_err());
        assert_eq!(current(&bin), Some(fs::canonicalize(&first).unwrap()));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reaping_keeps_the_current_and_anything_not_proven_unused() {
        let root = scratch("reap");
        let (pkg, bin) = (root.join("pkg"), root.join("bin"));
        let mut published = Vec::new();
        for version in [b"pi-a", b"pi-b", b"pi-c", b"pi-d"] {
            package(&pkg, version, b"wasm", None);
            published.push(publish(&bin, &pkg).unwrap());
        }
        let [stale, mapped, unknown, live] = published.as_slice() else {
            unreachable!()
        };
        reap_unused_with(&bin, |path| {
            if path == mapped.as_path() {
                MappedState::InUse
            } else if path == unknown.as_path() {
                MappedState::Unknown
            } else {
                MappedState::Unused
            }
        })
        .unwrap();
        assert!(!stale.exists(), "没人用的旧版本要删");
        assert!(mapped.is_file(), "还在跑的 Pi 用着的要留");
        assert!(unknown.is_file(), "证明不了没人用的要留");
        assert!(live.is_file(), "链接指着的当前版本要留");
        fs::remove_dir_all(root).unwrap();
    }
}
