//! App 放的位置能不能自更新，以及「移到『应用程序』」。
//!
//! 带着下载隔离标记、没用访达挪过位置就打开的 App，macOS 每次启动都把它挂到一个随机的
//! 只读目录里运行（App Translocation，路径形如 `…/AppTranslocation/<id>/d/Smelt.app`）；
//! 直接从 dmg 里打开也是只读卷。自更新要在 App 旁边写新版本，在这两种位置永远失败，
//! 每次启动都还是同一个结局。
//!
//! 做法同 LetsMove 和 Electron 的 `moveToApplicationsFolder`：启动时检测并提议移动。用户
//! 同意后拷到「应用程序」、去掉隔离标记、等这个进程退出后从新位置重开，转移前的那份移到
//! 废纸篓。去掉隔离标记发生在用户点了「移过去」之后，和用户自己用访达挪一次等价。

use std::ffi::{CString, c_void};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Component, Path, PathBuf};

use anyhow::Context as _;
use objc2::rc::Retained;
use objc2_foundation::{NSFileManager, NSString, NSURL};

const SYSTEM_APPLICATIONS: &str = "/Applications";
const QUARANTINE_XATTR: &str = "com.apple.quarantine";

/// App 现在的位置为什么装不上更新。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocationIssue {
    /// 被系统转移到只读的临时目录里运行。
    Translocated,
    /// 所在目录写不进去，比如直接从 dmg 里打开。
    ReadOnly,
}

/// 正在运行的这份 App 的位置问题。没问题，或者不在 `.app` 里运行（开发构建），返回 `None`。
pub fn current_issue() -> Option<LocationIssue> {
    let bundle = crate::updater::current_app_bundle().ok()?;
    let parent_writable = bundle.parent().is_some_and(is_writable_dir);
    issue_for(&bundle, parent_writable)
}

fn issue_for(bundle: &Path, parent_writable: bool) -> Option<LocationIssue> {
    if is_translocated(bundle) {
        Some(LocationIssue::Translocated)
    } else if !parent_writable {
        Some(LocationIssue::ReadOnly)
    } else {
        None
    }
}

fn is_translocated(bundle: &Path) -> bool {
    bundle
        .components()
        .any(|component| component == Component::Normal("AppTranslocation".as_ref()))
}

/// 只读卷上 `access` 报 `EROFS`，和没权限一样算写不进去。
fn is_writable_dir(dir: &Path) -> bool {
    let Ok(path) = CString::new(dir.as_os_str().as_bytes()) else {
        return false;
    };
    unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 }
}

/// 移到哪个目录：系统的「应用程序」写得进去就放那儿，否则放用户自己的 `~/Applications`，
/// 不要管理员权限。
fn destination_dir(system_writable: bool, home: &Path) -> PathBuf {
    if system_writable {
        PathBuf::from(SYSTEM_APPLICATIONS)
    } else {
        home.join("Applications")
    }
}

/// 把正在运行的这份 App 拷到「应用程序」，返回新位置。那里已有一份就先移到废纸篓。
/// 不退出、不重开：调用方先 [`relaunch_after_exit`]，再退出这个进程。
pub fn move_to_applications() -> anyhow::Result<PathBuf> {
    let bundle = crate::updater::current_app_bundle()?;
    let home = dirs::home_dir().context("找不到用户目录")?;
    let dir = destination_dir(is_writable_dir(Path::new(SYSTEM_APPLICATIONS)), &home);
    std::fs::create_dir_all(&dir).with_context(|| format!("建不了 {}", dir.display()))?;
    let name = bundle
        .file_name()
        .context("App 路径没有文件名")?
        .to_os_string();
    let destination = dir.join(&name);

    // 先拷到同目录的临时名再改名到位：拷到一半失败不会在「应用程序」里留下半个 App。
    let staging = dir.join(format!(
        ".{}.smelt-move-{}.app",
        Path::new(&name)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("Smelt"),
        uuid::Uuid::new_v4().simple()
    ));
    let placed = copy_bundle(&bundle, &staging)
        .and_then(|()| clear_quarantine(&staging))
        .and_then(|()| {
            if destination.exists() {
                move_to_trash(&destination).with_context(|| {
                    format!("{} 已有一份，移不到废纸篓", destination.display())
                })?;
            }
            std::fs::rename(&staging, &destination)
                .with_context(|| format!("放不进 {}", destination.display()))
        });
    if let Err(error) = placed {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }

    // 转移前的那份留着，以后从那儿打开的又是旧版、又被转移。查不到或移不动就算了。
    if is_translocated(&bundle)
        && let Some(original) = translocation_original(&bundle)
        && original != destination
        && let Err(error) = move_to_trash(&original)
    {
        smelt_sys::app_log::error(
            "app-location",
            &format!("原来那份 {} 移不到废纸篓：{error:#}", original.display()),
        );
    }
    Ok(destination)
}

/// 等这个进程退出后从 `app` 重新打开。交给一个单独的 shell 去等，GUI 退出时它还在；
/// 不能先开再退，新旧两个 GUI 会同时连着守护。
pub fn relaunch_after_exit(app: &Path) -> anyhow::Result<()> {
    std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"while kill -0 "$1" 2>/dev/null; do sleep 0.2; done; exec /usr/bin/open "$2""#)
        .arg("sh")
        .arg(std::process::id().to_string())
        .arg(app)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("安排重新打开失败")?;
    Ok(())
}

fn copy_bundle(source: &Path, destination: &Path) -> anyhow::Result<()> {
    let output = std::process::Command::new("/usr/bin/ditto")
        .arg(source)
        .arg(destination)
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "拷贝 App 失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// 包里没带这个属性的文件会让 `xattr -dr` 报错退出，所以不看它的退出码，只看包本身的
/// 属性去掉没有：Gatekeeper 按包顶层判断要不要转移。
fn clear_quarantine(bundle: &Path) -> anyhow::Result<()> {
    let _ = std::process::Command::new("/usr/bin/xattr")
        .args(["-dr", QUARANTINE_XATTR])
        .arg(bundle)
        .output()?;
    let still = std::process::Command::new("/usr/bin/xattr")
        .args(["-p", QUARANTINE_XATTR])
        .arg(bundle)
        .output()?;
    if still.status.success() {
        anyhow::bail!("去不掉 {} 的隔离标记", bundle.display());
    }
    Ok(())
}

/// 系统的「移到废纸篓」：和访达里删除一样能「放回原处」。
fn move_to_trash(path: &Path) -> anyhow::Result<()> {
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(&url, None)
        .map_err(|error| anyhow::anyhow!("{}", error.localizedDescription()))
}

/// 被转移的 App 原来放在哪。Security 框架的 `SecTranslocateCreateOriginalPathForURL`
/// 没进公开头文件（Sparkle、LetsMove 都这么查），按符号名动态取，取不到就当查不到。
fn translocation_original(bundle: &Path) -> Option<PathBuf> {
    type CreateOriginalPath =
        unsafe extern "C" fn(translocated: *const c_void, error: *mut *const c_void) -> *mut NSURL;

    let framework = c"/System/Library/Frameworks/Security.framework/Security";
    let symbol = c"SecTranslocateCreateOriginalPathForURL";
    let create = unsafe {
        let handle = libc::dlopen(framework.as_ptr(), libc::RTLD_LAZY);
        if handle.is_null() {
            return None;
        }
        let address = libc::dlsym(handle, symbol.as_ptr());
        if address.is_null() {
            return None;
        }
        std::mem::transmute::<*mut c_void, CreateOriginalPath>(address)
    };
    // NSURL 和 CFURL 免费桥接；返回值按 Create 规则归我们，交给 Retained 释放。
    let url = NSURL::fileURLWithPath(&NSString::from_str(&bundle.to_string_lossy()));
    let mut error: *const c_void = std::ptr::null();
    let original = unsafe {
        Retained::from_raw(create(Retained::as_ptr(&url).cast(), &mut error))
    };
    if !error.is_null() {
        #[link(name = "CoreFoundation", kind = "framework")]
        unsafe extern "C" {
            fn CFRelease(object: *const c_void);
        }
        unsafe { CFRelease(error) };
    }
    original?.path().map(|path| PathBuf::from(path.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 日志里那位用户的路径：每次启动都换一个随机目录，自更新在旁边写新版本时报只读。
    #[test]
    fn translocated_bundle_needs_moving_even_if_its_folder_looks_writable() {
        let bundle = Path::new(
            "/private/var/folders/y8/qg2j9s6j709ghp3yb99zgjd00000gq/T/AppTranslocation/\
             C2F09E77-0BCD-477F-AD0D-191B299A134E/d/Smelt.app",
        );
        assert_eq!(issue_for(bundle, true), Some(LocationIssue::Translocated));
    }

    #[test]
    fn read_only_folder_needs_moving() {
        let bundle = Path::new("/Volumes/Smelt/Smelt.app");
        assert_eq!(issue_for(bundle, false), Some(LocationIssue::ReadOnly));
    }

    /// 放在用户自己能写的地方（「应用程序」或别处）都能自更新，不打扰。
    #[test]
    fn writable_location_is_fine() {
        assert_eq!(issue_for(Path::new("/Applications/Smelt.app"), true), None);
        assert_eq!(
            issue_for(Path::new("/Users/me/Downloads/Smelt.app"), true),
            None
        );
    }

    /// 名字里带这个词的普通目录不算转移，只认路径里单独的一段。
    #[test]
    fn only_a_whole_path_component_counts_as_translocation() {
        let bundle = Path::new("/Users/me/AppTranslocationNotes/Smelt.app");
        assert_eq!(issue_for(bundle, true), None);
    }

    #[test]
    fn falls_back_to_user_applications_without_admin_rights() {
        let home = Path::new("/Users/me");
        assert_eq!(
            destination_dir(true, home),
            PathBuf::from("/Applications")
        );
        assert_eq!(
            destination_dir(false, home),
            PathBuf::from("/Users/me/Applications")
        );
    }
}
