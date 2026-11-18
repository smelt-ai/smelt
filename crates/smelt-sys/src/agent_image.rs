//! 编译好的 smelt-agent 一整套（可执行文件、photon wasm、`native/`、内置技能 `skills/`）装在
//! `~/.smelt/bin/agent/`，名字固定。Unix 上 `~/.smelt/bin/smelt-agent` 是指向
//! `agent/smelt-agent` 的链接，MCP 预设写的就是这条路径；Windows 上执行不了它，smelt-agent 起
//! MCP 时换成自己。
//!
//! 换版本时守护已经停了，它起的 Pi 也一起收掉了（桌面和更新流程都是先停再装）。Linux 运行时
//! 自己升级时还在跑：整套先写进同目录的临时目录，再两次 rename 换进来，旧 Pi 握着的文件
//! 不受影响，随后守护重启把它们收掉。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::daemon_image::content_fingerprint;

/// Unix 上的启动链接名，也是 Unix 上的可执行文件名。Windows 上可执行文件是
/// `smelt-agent.exe`，见 [`binary_file_name`]。
pub const LAUNCH_NAME: &str = "smelt-agent";
/// 装好的一整套所在的目录名。
pub const INSTALL_DIR: &str = "agent";

pub fn binary_file_name() -> &'static str {
    if cfg!(windows) {
        "smelt-agent.exe"
    } else {
        LAUNCH_NAME
    }
}
const WASM_NAME: &str = "photon_rs_bg.wasm";
const NATIVE_DIR: &str = "native";
/// 内置技能目录。只在智能体勾选时用 `--skill` 传给 Pi，不在 Pi 的自动发现路径上。
pub const SKILLS_DIR: &str = "skills";
/// App 里这一整套相对 `Contents` 的位置。wasm 不是代码，不能放进只收签名代码的 `Contents/MacOS`。
pub const APP_BUNDLE_DIR: &str = "Resources/smelt-agent";
/// 装好的目录里记着这一套的指纹，判断要不要重装时不用再读一遍装好的那份。最后写进临时
/// 目录、随目录一起 rename 进来，有它就说明这一套是完整装好的。
const FINGERPRINT_FILE: &str = ".bundle-sha256";

/// 一套待装的 smelt-agent（App 里那份、Linux 包里那份）。读的时候算好指纹：要读一遍约 120MB。
pub struct Bundle {
    src: PathBuf,
    /// 相对 `src`、排好序。符号链接一律不跟。
    files: Vec<PathBuf>,
    fingerprint: String,
}

impl Bundle {
    /// smelt-agent 和 wasm 必须有，`native/`、`skills/` 可选。
    pub fn read(src_dir: &Path) -> io::Result<Self> {
        let mut files = Vec::new();
        for required in [binary_file_name(), WASM_NAME] {
            if !fs::symlink_metadata(src_dir.join(required)).is_ok_and(|meta| meta.is_file()) {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("missing {}", src_dir.join(required).display()),
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
        let mut digest = Sha256::new();
        for relative in &files {
            digest.update(relative.to_string_lossy().as_bytes());
            digest.update([0]);
            digest.update(content_fingerprint(&src_dir.join(relative))?.as_bytes());
            digest.update(*b"\n");
        }
        Ok(Self {
            src: src_dir.to_path_buf(),
            files,
            fingerprint: hex::encode(digest.finalize()),
        })
    }

    /// `dir` 里装着的就是这一套。
    pub fn is_installed_in(&self, dir: &Path) -> bool {
        current(dir).is_some()
            && fs::read_to_string(dir.join(INSTALL_DIR).join(FINGERPRINT_FILE))
                .is_ok_and(|text| text.trim() == self.fingerprint)
    }

    /// 装进 `dir/agent`，已经是这一套就只补链接。返回装好的可执行文件。
    pub fn install(&self, dir: &Path) -> io::Result<PathBuf> {
        if !self.is_installed_in(dir) {
            fs::create_dir_all(dir)?;
            let staging = dir.join(format!(
                ".{INSTALL_DIR}.new-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4().simple()
            ));
            let result = self
                .write_tree(&staging)
                .and_then(|()| swap_in(dir, &staging));
            if result.is_err() {
                let _ = fs::remove_dir_all(&staging);
            }
            result?;
        }
        #[cfg(unix)]
        link(dir)?;
        Ok(dir.join(INSTALL_DIR).join(binary_file_name()))
    }

    fn write_tree(&self, staging: &Path) -> io::Result<()> {
        for relative in &self.files {
            let dest = staging.join(relative);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(self.src.join(relative), &dest)?;
            sync_file(&dest)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(
                staging.join(binary_file_name()),
                fs::Permissions::from_mode(0o755),
            )?;
        }
        let stamp = staging.join(FINGERPRINT_FILE);
        fs::write(&stamp, &self.fingerprint)?;
        sync_file(&stamp)
    }
}

fn sync_file(path: &Path) -> io::Result<()> {
    // Windows 的 FlushFileBuffers 要求写权限。只读打开会返回拒绝访问。
    fs::OpenOptions::new()
        .read(true)
        .write(cfg!(windows))
        .open(path)?
        .sync_all()
}

/// 把写好的 `staging` 换成 `dir/agent`：旧目录先挪开，新目录改名进来，再删旧目录。新目录
/// 进不来就把旧的挪回去。旧目录删不掉（Windows 上还有进程占着）就留着，守护下次启动时清。
fn swap_in(dir: &Path, staging: &Path) -> io::Result<()> {
    let target = dir.join(INSTALL_DIR);
    let old = dir.join(format!(
        ".{INSTALL_DIR}.old-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let moved_old = match fs::rename(&target, &old) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error),
    };
    if let Err(error) = fs::rename(staging, &target) {
        if moved_old {
            let _ = fs::rename(&old, &target);
        }
        return Err(error);
    }
    if moved_old {
        let _ = fs::remove_dir_all(&old);
    }
    Ok(())
}

/// `dir/smelt-agent` → `agent/smelt-agent`。已经是就不动，否则换一条新链接盖过去
/// （包括旧布局里指向 `smelt-agent-<指纹>/` 的那条）。
#[cfg(unix)]
fn link(dir: &Path) -> io::Result<()> {
    let target = Path::new(INSTALL_DIR).join(binary_file_name());
    let link = dir.join(LAUNCH_NAME);
    if fs::read_link(&link).is_ok_and(|current| current == target) {
        return Ok(());
    }
    let tmp = dir.join(format!(
        ".{LAUNCH_NAME}.link-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let result = std::os::unix::fs::symlink(&target, &tmp).and_then(|()| fs::rename(&tmp, &link));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn collect_regular_files(root: &Path, dir: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            collect_regular_files(root, &path, files)?;
        } else if meta.is_file() {
            files.push(
                path.strip_prefix(root)
                    .map_err(io::Error::other)?
                    .to_path_buf(),
            );
        }
    }
    Ok(())
}

/// `dir` 里装好的、能执行的 smelt-agent。没装都是 `None`。
pub fn current(dir: &Path) -> Option<PathBuf> {
    let executable = dir.join(INSTALL_DIR).join(binary_file_name());
    let meta = fs::metadata(&executable).ok()?;
    if !meta.is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if meta.permissions().mode() & 0o111 == 0 {
            return None;
        }
    }
    Some(executable)
}

/// `src_dir` 里有没有一整套可以装的 smelt-agent（不跟符号链接）。
pub fn bundled(src_dir: &Path) -> bool {
    fs::symlink_metadata(src_dir.join(binary_file_name())).is_ok_and(|meta| meta.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "smelt-agent-image-{label}-{}-{}",
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

    fn install(bin: &Path, pkg: &Path) -> io::Result<PathBuf> {
        Bundle::read(pkg)?.install(bin)
    }

    /// 装好的位置固定；换一套就原地换掉，不留旧目录、不留临时目录。
    #[test]
    fn a_new_bundle_replaces_the_installed_one_in_place() {
        let root = scratch("replace");
        let (pkg, bin) = (root.join("pkg"), root.join("bin"));
        package(&pkg, b"pi-v1", b"wasm", Some(b"native-v1"));
        let first = install(&bin, &pkg).unwrap();
        assert_eq!(first, bin.join(INSTALL_DIR).join(binary_file_name()));
        assert_eq!(current(&bin), Some(first.clone()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(&first).unwrap().permissions().mode();
            assert_ne!(mode & 0o111, 0, "smelt-agent 必须可执行");
            assert_eq!(
                fs::canonicalize(bin.join(LAUNCH_NAME)).unwrap(),
                fs::canonicalize(&first).unwrap(),
                "启动链接指向装好的那份"
            );
        }

        package(&pkg, b"pi-v1", b"wasm", Some(b"native-v2"));
        let second = install(&bin, &pkg).unwrap();
        assert_eq!(second, first);
        assert_eq!(
            fs::read(bin.join(INSTALL_DIR).join("native/sub/addon.node")).unwrap(),
            b"native-v2"
        );
        let names: Vec<String> = fs::read_dir(&bin)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.iter().all(|name| !name.starts_with('.')),
            "旧目录和临时目录都要清掉：{names:?}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// 只改了内置技能也是新的一套：智能体勾选时传的是装好的那份。
    #[test]
    fn a_skill_only_change_is_a_new_bundle() {
        let root = scratch("skills");
        let (pkg, bin) = (root.join("pkg"), root.join("bin"));
        package(&pkg, b"pi-v1", b"wasm", None);
        fs::create_dir_all(pkg.join("skills/smelt")).unwrap();
        fs::write(pkg.join("skills/smelt/SKILL.md"), "v1").unwrap();
        install(&bin, &pkg).unwrap();
        assert!(Bundle::read(&pkg).unwrap().is_installed_in(&bin));

        fs::write(pkg.join("skills/smelt/SKILL.md"), "v2").unwrap();
        let bundle = Bundle::read(&pkg).unwrap();
        assert!(!bundle.is_installed_in(&bin));
        bundle.install(&bin).unwrap();
        let skill = bin.join(INSTALL_DIR).join("skills/smelt/SKILL.md");
        assert_eq!(fs::read_to_string(skill).unwrap(), "v2");
        fs::remove_dir_all(root).unwrap();
    }

    /// 守护起 Pi、Linux 运行时启动时补齐，都看这一个答案。
    #[test]
    fn current_is_none_until_installed() {
        let root = scratch("current");
        let (pkg, bin) = (root.join("pkg"), root.join("bin"));
        fs::create_dir_all(&bin).unwrap();
        assert!(current(&bin).is_none(), "没装过");
        assert!(!bundled(&pkg));
        package(&pkg, b"pi-v1", b"wasm", None);
        assert!(bundled(&pkg));
        let installed = install(&bin, &pkg).unwrap();
        assert_eq!(current(&bin), Some(installed));
        assert!(
            !bundled(&bin),
            "bin 里的 smelt-agent 是链接，不能当成一套包再装"
        );
        fs::remove_dir_all(bin.join(INSTALL_DIR)).unwrap();
        assert!(current(&bin).is_none(), "目录没了");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_package_without_wasm_is_refused_and_the_installed_one_stays() {
        let root = scratch("no-wasm");
        let (pkg, bin) = (root.join("pkg"), root.join("bin"));
        package(&pkg, b"pi-v1", b"wasm", None);
        let first = install(&bin, &pkg).unwrap();
        fs::write(pkg.join(binary_file_name()), b"pi-v2").unwrap();
        fs::remove_file(pkg.join(WASM_NAME)).unwrap();
        assert!(install(&bin, &pkg).is_err());
        assert_eq!(fs::read(&first).unwrap(), b"pi-v1");
        fs::remove_dir_all(root).unwrap();
    }

    /// 没有指纹记录的目录（中途断掉、旧布局迁过来的）不算装好，下次照装。
    #[test]
    fn a_dir_without_the_stamp_is_reinstalled() {
        let root = scratch("stamp");
        let (pkg, bin) = (root.join("pkg"), root.join("bin"));
        package(&pkg, b"pi-v1", b"wasm", None);
        install(&bin, &pkg).unwrap();
        fs::remove_file(bin.join(INSTALL_DIR).join(FINGERPRINT_FILE)).unwrap();
        assert!(!Bundle::read(&pkg).unwrap().is_installed_in(&bin));
        install(&bin, &pkg).unwrap();
        assert!(Bundle::read(&pkg).unwrap().is_installed_in(&bin));
        fs::remove_dir_all(root).unwrap();
    }
}
