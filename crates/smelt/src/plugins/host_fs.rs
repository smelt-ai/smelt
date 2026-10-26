//! `smelt-fs`：smelt 注册给全部插件的通用文件 HostModule。
//!
//! 插件当作可信代码，不限权限（见 `mod.rs`），这里也不设路径门控。不按插件区分，
//! 也不含任何某个插件专属的逻辑。
//!
//! 只放官方 `fs/promises` 没有或做不对的操作：
//! - 官方没有：`symlink`、`realpath`、`copy`（整树复制）、`exchange`（原子交换）。
//! - 官方做不对：拿到绝对路径时，官方会把整条路径连同最后一段的软链一起解开
//!   （gpui-shell `capability.rs` 的 `resolved_root`），于是对一条有效软链的
//!   `lstat`/`readlink` 看到的是目标，`unlink`/`rename` 落到目标上。这几个作用于
//!   「路径本身」的操作放在这里，上游修好后换回官方的。
//!
//! 路径规则（为了语义明确，不是为了拦权限）：
//! - 路径必须是绝对路径，且不含 `..`：插件没有当前目录，相对路径没有确定的含义。
//! - 作用于路径本身的调用（lstat、readlink、symlink 的落点、rename、unlink、
//!   exchange、copy 的目标）只规范化父目录，最后一段不跟随——否则无法区分本体与引用。
//! - 规范化途中遇到失效软链一律拒绝，不把它当作「还不存在的目录」顺着往下写。

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::time::UNIX_EPOCH;

use gpui_shell::{HostArguments, HostError, HostModule, HostObject, HostResult, HostValue};

/// 全部插件共用的文件操作。
pub(super) struct HostFs;

impl HostFs {
    /// 作用于路径本身：父目录规范化，最后一段原样保留。
    fn nofollow(&self, raw: &str) -> Result<PathBuf, HostError> {
        let path = lexical(raw)?;
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return Err(HostError::new(format!("不能操作根目录：{raw}")));
        };
        Ok(canonical_prefix(parent)?.join(name))
    }

    /// 跟随软链。目标不存在时返回 `None`。
    fn follow(&self, raw: &str) -> Result<Option<PathBuf>, HostError> {
        let at = self.nofollow(raw)?;
        Ok(fs::canonicalize(&at).ok())
    }

    fn lstat(&self, raw: &str) -> HostResult {
        let at = self.nofollow(raw)?;
        Ok(fs::symlink_metadata(&at).map_or(HostValue::Null, |meta| stat_value(&meta)))
    }

    fn readlink(&self, raw: &str) -> HostResult {
        let at = self.nofollow(raw)?;
        Ok(fs::read_link(&at).map_or(HostValue::Null, |target| path_value(&target)))
    }

    fn realpath(&self, raw: &str) -> HostResult {
        Ok(self
            .follow(raw)?
            .map_or(HostValue::Null, |real| path_value(&real)))
    }

    /// 在 `at` 建一条指向 `target` 的软链。
    fn symlink(&self, target: &str, at: &str) -> HostResult {
        let at = self.nofollow(at)?;
        let target = lexical(target)?;
        symlink_at(&target, &at)
            .map(|()| HostValue::Null)
            .map_err(|error| HostError::new(format!("创建链接 {} 失败：{error}", at.display())))
    }

    /// 移动文件、目录或软链本身。目标已存在时拒绝，不静默覆盖。
    fn rename(&self, from: &str, to: &str) -> HostResult {
        let from = self.nofollow(from)?;
        let to = self.nofollow(to)?;
        if fs::symlink_metadata(&to).is_ok() {
            return Err(HostError::new(format!("目标已存在：{}", to.display())));
        }
        match fs::rename(&from, &to) {
            Ok(()) => Ok(HostValue::Null),
            // 同卷 rename 是原子的；跨卷时退成复制再删，语义仍是「搬走」，不是留下两份。
            Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {
                relocate_across_devices(&from, &to)?;
                Ok(HostValue::Null)
            }
            Err(error) => Err(HostError::new(format!(
                "移动 {} → {} 失败：{error}",
                from.display(),
                to.display()
            ))),
        }
    }

    /// 只删文件或软链本身。删目录请用官方 `rmdir`（只删空目录），不提供递归删除。
    fn unlink(&self, raw: &str) -> HostResult {
        let at = self.nofollow(raw)?;
        let meta = fs::symlink_metadata(&at)
            .map_err(|error| HostError::new(format!("{} 不存在：{error}", at.display())))?;
        if meta.is_dir() {
            return Err(HostError::new(format!(
                "{} 是目录，unlink 只删文件或软链",
                at.display()
            )));
        }
        remove_file_or_link(&at, meta.file_type())
            .map(|()| HostValue::Null)
            .map_err(|error| HostError::new(format!("删除 {} 失败：{error}", at.display())))
    }

    /// 原子交换两个已存在的路径。用来整体替换一个目录：先在旁边备好新版本再交换，
    /// 指向原路径的软链全程不悬空。
    fn exchange(&self, left: &str, right: &str) -> HostResult {
        let left = self.nofollow(left)?;
        let right = self.nofollow(right)?;
        for path in [&left, &right] {
            if fs::symlink_metadata(path).is_err() {
                return Err(HostError::new(format!("{} 不存在", path.display())));
            }
        }
        exchange_paths(&left, &right)?;
        Ok(HostValue::Null)
    }

    /// `copy` 的同步半截：在调用方的线程上做完全部检查，返回要交给后台的源和目标。
    fn copy_plan(&self, source: &str, destination: &str) -> Result<(PathBuf, PathBuf), HostError> {
        let source = self
            .follow(source)?
            .ok_or_else(|| HostError::new(format!("复制源不存在：{source}")))?;
        let destination = self.nofollow(destination)?;
        if fs::symlink_metadata(&destination).is_ok() {
            return Err(HostError::new(format!(
                "目标已存在：{}",
                destination.display()
            )));
        }
        let parent = destination
            .parent()
            .ok_or_else(|| HostError::new("不能复制到根目录"))?;
        if !parent.is_dir() {
            return Err(HostError::new(format!(
                "目标的上级目录不存在：{}",
                parent.display()
            )));
        }
        if same_or_nested(&source, &destination) {
            return Err(HostError::new("源和目标相同或互相包含，拒绝复制"));
        }
        Ok((source, destination))
    }
}

const FS_DECLARATIONS: &str = r#"
export type EntryKind = "dir" | "file" | "symlink" | "other";
export interface Stat { kind: EntryKind; mtime_ms: number }
/** 不跟随软链。不存在时为 null。 */
export function lstat(path: string): Stat | null;
/** 软链里存的目标原文。不是软链或不存在时为 null。 */
export function readlink(path: string): string | null;
/** 跟随软链后的真实路径。不存在（含失效软链）时为 null。 */
export function realpath(path: string): string | null;
/** 在 at 建一条指向 target 的软链。 */
export function symlink(target: string, at: string): null;
/** 移动文件、目录或软链本身。目标已存在时抛错。 */
export function rename(from: string, to: string): null;
/** 删除文件或软链本身。目录请用 fs/promises 的 rmdir。 */
export function unlink(path: string): null;
/** 原子交换两个已存在的路径。 */
export function exchange(left: string, right: string): null;
export interface CopyOptions {
  /** 源里的软链：preserve 按原文重建（默认），skip 跳过不复制。 */
  symlinks?: "preserve" | "skip";
}
/** 复制文件或整棵目录到一个还不存在的路径，在后台线程执行。失败时不留半成品。 */
export function copy(source: string, destination: string, options?: CopyOptions): Promise<null>;
"#;

fn one(arguments: &HostArguments) -> Result<&str, HostError> {
    arguments.string(0)
}

/// `copy` 第三个参数里的 `symlinks`。缺省是保留。
fn skip_symlinks(arguments: &HostArguments) -> Result<bool, HostError> {
    let Some(options) = arguments.get(2).filter(|value| !value.is_null()) else {
        return Ok(false);
    };
    match options.get("symlinks").and_then(HostValue::as_str) {
        None | Some("preserve") => Ok(false),
        Some("skip") => Ok(true),
        Some(other) => Err(HostError::new(format!(
            "copy 的 symlinks 只能是 preserve 或 skip，收到 {other}"
        ))),
    }
}

pub(super) fn fs_module(host: Rc<HostFs>) -> HostModule {
    let module = HostModule::new("smelt-fs").declarations(FS_DECLARATIONS);
    let register = |module: HostModule,
                    name: &str,
                    call: fn(&HostFs, &HostArguments) -> HostResult|
     -> HostModule {
        let host = host.clone();
        module.function(name, move |arguments| call(&host, arguments))
    };
    let module = register(module, "lstat", |host, args| host.lstat(one(args)?));
    let module = register(module, "readlink", |host, args| host.readlink(one(args)?));
    let module = register(module, "realpath", |host, args| host.realpath(one(args)?));
    let module = register(module, "symlink", |host, args| {
        host.symlink(args.string(0)?, args.string(1)?)
    });
    let module = register(module, "rename", |host, args| {
        host.rename(args.string(0)?, args.string(1)?)
    });
    let module = register(module, "unlink", |host, args| host.unlink(one(args)?));
    let module = register(module, "exchange", |host, args| {
        host.exchange(args.string(0)?, args.string(1)?)
    });
    module.async_function("copy", move |arguments| {
        let (source, destination) = host.copy_plan(arguments.string(0)?, arguments.string(1)?)?;
        let skip = skip_symlinks(arguments)?;
        Ok(async move { copy_entry(&source, &destination, skip).map(|()| HostValue::Null) })
    })
}

/// 绝对路径、去掉 `.`、拒绝 `..`。不碰磁盘。
///
/// `\\?\` 路径的 `components` 只认 `\`。`a/../b` 因此是一个普通名字，不会变成
/// `ParentDir`。门控按两种分隔符切开，这种写法同样拒绝。
fn lexical(raw: &str) -> Result<PathBuf, HostError> {
    if raw.split(['/', '\\']).any(|segment| segment == "..") {
        return Err(HostError::new(format!("路径不能含 `..`：{raw}")));
    }
    let path = Path::new(raw);
    if !path.is_absolute() {
        return Err(HostError::new(format!("需要绝对路径：{raw}")));
    }
    let mut cleaned = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(HostError::new(format!("路径不能含 `..`：{raw}")));
            }
            Component::Normal(name) if name == "." => {}
            other => cleaned.push(other),
        }
    }
    Ok(cleaned.into_iter().collect())
}

/// 删文件，或软链本身。
///
/// Windows 上目录软链带目录属性，`DeleteFile` 会返回 Access Denied。这种链接用
/// `RemoveDirectory` 删，删的是链接，不是它指向的目录。
fn remove_file_or_link(path: &Path, file_type: fs::FileType) -> std::io::Result<()> {
    #[cfg(windows)]
    if is_directory_link(&file_type) {
        return fs::remove_dir(path);
    }
    let _ = file_type;
    fs::remove_file(path)
}

#[cfg(windows)]
fn is_directory_link(file_type: &fs::FileType) -> bool {
    use std::os::windows::fs::FileTypeExt;
    file_type.is_symlink_dir()
}

/// 把最深的已存在祖先规范化，其余段原样拼回（它们不存在，也就没有软链可跟）。
///
/// 中途碰到失效软链（lstat 得到、canonicalize 不了）直接拒绝：否则它会被当成
/// 「还不存在的目录」，随后的写操作顺着它落到链接原本指向的地方。
fn canonical_prefix(path: &Path) -> Result<PathBuf, HostError> {
    let mut existing = path;
    let mut missing = Vec::new();
    loop {
        match fs::canonicalize(existing) {
            Ok(real) => {
                let mut out = real;
                for name in missing.iter().rev() {
                    out.push(name);
                }
                return Ok(out);
            }
            Err(_) if fs::symlink_metadata(existing).is_ok() => {
                return Err(HostError::new(format!(
                    "路径经过失效的软链：{}",
                    existing.display()
                )));
            }
            Err(_) => match (existing.parent(), existing.file_name()) {
                (Some(parent), Some(name)) => {
                    missing.push(name.to_owned());
                    existing = parent;
                }
                _ => return Ok(path.to_path_buf()),
            },
        }
    }
}

fn same_or_nested(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}

/// 复制一个文件或一整棵目录到还不存在的 `destination`。失败时把这次建出来的东西删掉；
/// 目标若在检查之后被别人抢先建了，创建这一步就会失败，不会去删别人的东西。
fn copy_entry(source: &Path, destination: &Path, skip_symlinks: bool) -> Result<(), HostError> {
    let meta = fs::symlink_metadata(source)
        .map_err(|error| HostError::new(format!("{} 不存在：{error}", source.display())))?;
    if meta.is_dir() {
        fs::create_dir(destination).map_err(|error| {
            HostError::new(format!("创建 {} 失败：{error}", destination.display()))
        })?;
        let copied = copy_tree_into(source, destination, skip_symlinks);
        if copied.is_err() {
            let _ = fs::remove_dir_all(destination);
        }
        copied
    } else if meta.is_file() {
        let mut from = fs::File::open(source)
            .map_err(|error| HostError::new(format!("读取 {} 失败：{error}", source.display())))?;
        let mut to = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)
            .map_err(|error| {
                HostError::new(format!("创建 {} 失败：{error}", destination.display()))
            })?;
        let copied = std::io::copy(&mut from, &mut to)
            .and_then(|_| to.set_permissions(meta.permissions()))
            .map_err(|error| HostError::new(format!("复制 {} 失败：{error}", source.display())));
        if copied.is_err() {
            let _ = fs::remove_file(destination);
        }
        copied
    } else {
        Err(HostError::new(format!("无法复制 {}", source.display())))
    }
}

/// 把 `source` 目录里的条目复制进已经建好的 `destination`。软链按链接原文重建，
/// 不跟随；`skip_symlinks` 时直接跳过。
fn copy_tree_into(source: &Path, destination: &Path, skip_symlinks: bool) -> Result<(), HostError> {
    let entries = fs::read_dir(source)
        .map_err(|error| HostError::new(format!("读取 {} 失败：{error}", source.display())))?;
    for entry in entries {
        let entry = entry
            .map_err(|error| HostError::new(format!("读取 {} 失败：{error}", source.display())))?;
        let file_type = entry.file_type().map_err(|error| {
            HostError::new(format!("读取 {} 失败：{error}", entry.path().display()))
        })?;
        let to = destination.join(entry.file_name());
        if file_type.is_symlink() {
            if skip_symlinks {
                continue;
            }
            let target = fs::read_link(entry.path()).map_err(|error| {
                HostError::new(format!("读取链接 {} 失败：{error}", entry.path().display()))
            })?;
            symlink_at(&target, &to).map_err(|error| {
                HostError::new(format!("创建链接 {} 失败：{error}", to.display()))
            })?;
        } else if file_type.is_dir() {
            fs::create_dir(&to)
                .map_err(|error| HostError::new(format!("创建 {} 失败：{error}", to.display())))?;
            copy_tree_into(&entry.path(), &to, skip_symlinks)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), &to).map_err(|error| {
                HostError::new(format!("复制 {} 失败：{error}", entry.path().display()))
            })?;
        } else {
            return Err(HostError::new(format!(
                "无法复制 {}",
                entry.path().display()
            )));
        }
    }
    Ok(())
}

/// 跨卷搬家：软链按链接文本重建，不跟随。调用时目标必须还不存在。
fn relocate_across_devices(from: &Path, to: &Path) -> Result<(), HostError> {
    let meta = fs::symlink_metadata(from)
        .map_err(|error| HostError::new(format!("{} 不存在：{error}", from.display())))?;
    if meta.file_type().is_symlink() {
        let target = fs::read_link(from).map_err(|error| {
            HostError::new(format!("读取链接 {} 失败：{error}", from.display()))
        })?;
        symlink_at(&target, to)
            .map_err(|error| HostError::new(format!("创建链接 {} 失败：{error}", to.display())))?;
        return remove_file_or_link(from, meta.file_type())
            .map_err(|error| HostError::new(format!("删除 {} 失败：{error}", from.display())));
    }
    copy_entry(from, to, false)?;
    let removed = if meta.is_dir() {
        fs::remove_dir_all(from)
    } else {
        fs::remove_file(from)
    };
    removed.map_err(|error| HostError::new(format!("删除 {} 失败：{error}", from.display())))
}

/// 建一条软链。Unix 用 `symlink`；Windows 用 `symlink_file` / `symlink_dir`，
/// 失败（没开开发者模式或没有管理员权限）按不支持报错（设计文档 D4）。
fn symlink_at(target: &Path, at: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        return std::os::unix::fs::symlink(target, at);
    }
    #[cfg(windows)]
    {
        // 目录和文件是两种链接。相对目标相对落点的父目录看；看不清（悬空链接）
        // 时按文件建。系统拒绝（没有创建链接的权限）就按不支持报。
        let resolved = if target.is_relative() {
            at.parent()
                .map(|parent| parent.join(target))
                .unwrap_or_else(|| target.to_path_buf())
        } else {
            target.to_path_buf()
        };
        let is_dir = std::fs::metadata(&resolved).is_ok_and(|meta| meta.is_dir());
        let result = if is_dir {
            std::os::windows::fs::symlink_dir(target, at)
        } else {
            std::os::windows::fs::symlink_file(target, at)
        };
        return result.map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                format!("创建软链接失败：{error}"),
            )
        });
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, at);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "当前平台不支持创建软链接",
        ))
    }
}

/// 交换两个已存在的路径。普通 rename 做不到「两边都在，且中间没有空窗」。
/// macOS / Linux 走原子交换；其它平台按不支持报错（设计文档 D4）。
fn exchange_paths(left: &Path, right: &Path) -> Result<(), HostError> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let left = CString::new(left.as_os_str().as_bytes())
            .map_err(|_| HostError::new("路径含空字节"))?;
        let right = CString::new(right.as_os_str().as_bytes())
            .map_err(|_| HostError::new("路径含空字节"))?;
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
            Err(HostError::new(format!(
                "原子交换失败：{}",
                std::io::Error::last_os_error()
            )))
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (left, right);
        Err(HostError::new("原子交换失败：当前平台不支持"))
    }
}

fn kind_of(file_type: fs::FileType) -> &'static str {
    if file_type.is_symlink() {
        "symlink"
    } else if file_type.is_dir() {
        "dir"
    } else if file_type.is_file() {
        "file"
    } else {
        "other"
    }
}

fn stat_value(meta: &fs::Metadata) -> HostValue {
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0.0, |duration| duration.as_millis() as f64);
    HostObject::new()
        .field("kind", kind_of(meta.file_type()))
        .field("mtime_ms", mtime_ms)
        .into()
}

pub(super) fn path_value(path: &Path) -> HostValue {
    HostValue::Str(path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 临时目录充当 home，旁边另建一个目录充当「主目录外」。canonicalize 一下：
    /// macOS 的临时目录在 /private 下有软链前缀。
    struct Sandbox {
        root: PathBuf,
        home: PathBuf,
        outside: PathBuf,
    }

    impl Sandbox {
        fn new() -> Self {
            let root = std::env::temp_dir()
                .join(format!("smelt-plugin-fs-{}", uuid::Uuid::new_v4().simple()));
            fs::create_dir_all(root.join("home")).unwrap();
            fs::create_dir_all(root.join("outside")).unwrap();
            let root = fs::canonicalize(&root).unwrap();
            Self {
                home: root.join("home"),
                outside: root.join("outside"),
                root,
            }
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.home.join(relative)
        }

        fn s(&self, relative: &str) -> String {
            self.path(relative).to_string_lossy().into_owned()
        }

        fn host(&self) -> HostFs {
            HostFs
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    /// `canonicalize` 在 Windows 上带 `\\?\` 前缀，`read_link` 读回来没有。
    /// 比较的是同一条路径，不是前缀字节。
    fn comparable_path(value: &HostValue) -> &str {
        let text = value.as_str().unwrap_or("");
        text.strip_prefix(r"\\?\").unwrap_or(text)
    }

    fn kind(value: HostValue) -> Option<String> {
        value
            .get("kind")
            .and_then(HostValue::as_str)
            .map(str::to_owned)
    }

    #[test]
    fn paths_must_be_absolute_without_parent_segments() {
        let sandbox = Sandbox::new();
        let host = sandbox.host();
        assert!(host.lstat("relative").is_err());
        let escape = format!("{}/../outside", sandbox.s("a"));
        assert!(host.lstat(&escape).is_err());
    }


    #[test]
    fn lstat_readlink_and_unlink_act_on_the_link_itself() {
        let sandbox = Sandbox::new();
        let host = sandbox.host();
        fs::create_dir_all(sandbox.path("real")).unwrap();
        host.symlink(&sandbox.s("real"), &sandbox.s("link"))
            .unwrap();

        assert_eq!(
            kind(host.lstat(&sandbox.s("link")).unwrap()).as_deref(),
            Some("symlink")
        );
        assert_eq!(
            comparable_path(&host.readlink(&sandbox.s("link")).unwrap()),
            comparable_path(&HostValue::Str(sandbox.s("real")))
        );
        assert_eq!(
            comparable_path(&host.realpath(&sandbox.s("link")).unwrap()),
            comparable_path(&HostValue::Str(sandbox.s("real")))
        );
        host.unlink(&sandbox.s("link")).unwrap();
        assert!(fs::symlink_metadata(sandbox.path("link")).is_err());
        assert!(sandbox.path("real").is_dir(), "删的是链接，目标还在");
        assert!(host.unlink(&sandbox.s("real")).is_err(), "unlink 不删目录");
    }

    /// 不限权限：主目录外的路径和指向主目录外的链接都照常操作。
    #[cfg(unix)]
    #[test]
    fn paths_and_links_outside_home_work() {
        let sandbox = Sandbox::new();
        let host = sandbox.host();
        let outside = sandbox.outside.to_string_lossy().into_owned();
        assert_eq!(kind(host.lstat(&outside).unwrap()).as_deref(), Some("dir"));
        host.symlink(&outside, &sandbox.s("out")).unwrap();
        assert_eq!(
            comparable_path(&host.realpath(&sandbox.s("out")).unwrap()),
            outside.as_str()
        );
        host.rename(&sandbox.s("out"), &sandbox.outside.join("moved").to_string_lossy())
            .unwrap();
        assert_eq!(
            kind(host.lstat(&sandbox.outside.join("moved").to_string_lossy()).unwrap()).as_deref(),
            Some("symlink")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_link_reads_as_missing_and_is_never_walked_through() {
        let sandbox = Sandbox::new();
        let host = sandbox.host();
        std::os::unix::fs::symlink(sandbox.outside.join("ghost"), sandbox.path("dangling"))
            .unwrap();
        assert!(host.realpath(&sandbox.s("dangling")).unwrap().is_null());
        assert!(!host.lstat(&sandbox.s("dangling")).unwrap().is_null());
        assert!(host.lstat(&sandbox.s("dangling/child")).is_err());
        host.unlink(&sandbox.s("dangling")).unwrap();
    }

    #[test]
    fn rename_refuses_to_overwrite() {
        let sandbox = Sandbox::new();
        let host = sandbox.host();
        fs::create_dir_all(sandbox.path("a")).unwrap();
        fs::create_dir_all(sandbox.path("b")).unwrap();
        assert!(host.rename(&sandbox.s("a"), &sandbox.s("b")).is_err());
        host.rename(&sandbox.s("a"), &sandbox.s("c")).unwrap();
        assert!(sandbox.path("c").is_dir());
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn exchange_swaps_two_directories_without_breaking_a_link() {
        let sandbox = Sandbox::new();
        let host = sandbox.host();
        fs::create_dir_all(sandbox.path("live")).unwrap();
        fs::write(sandbox.path("live/SKILL.md"), "old").unwrap();
        fs::create_dir_all(sandbox.path("staged")).unwrap();
        fs::write(sandbox.path("staged/SKILL.md"), "new").unwrap();
        host.symlink(&sandbox.s("live"), &sandbox.s("bridge"))
            .unwrap();

        host.exchange(&sandbox.s("staged"), &sandbox.s("live"))
            .unwrap();
        assert_eq!(
            fs::read_to_string(sandbox.path("bridge/SKILL.md")).unwrap(),
            "new"
        );
        assert_eq!(
            fs::read_to_string(sandbox.path("staged/SKILL.md")).unwrap(),
            "old"
        );
        assert!(
            host.exchange(&sandbox.s("live"), &sandbox.s("missing"))
                .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn copy_preserves_or_skips_symlinks() {
        let sandbox = Sandbox::new();
        let host = sandbox.host();
        let source = sandbox.path("source");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::write(source.join("SKILL.md"), "body").unwrap();
        fs::write(source.join("nested/note.txt"), "note").unwrap();
        std::os::unix::fs::symlink("SKILL.md", source.join("alias")).unwrap();

        let (from, to) = host
            .copy_plan(&sandbox.s("source"), &sandbox.s("kept"))
            .unwrap();
        copy_entry(&from, &to, false).unwrap();
        assert_eq!(
            fs::read_to_string(sandbox.path("kept/nested/note.txt")).unwrap(),
            "note"
        );
        assert_eq!(
            fs::read_link(sandbox.path("kept/alias")).unwrap(),
            PathBuf::from("SKILL.md")
        );

        let (from, to) = host
            .copy_plan(&sandbox.s("source"), &sandbox.s("plain"))
            .unwrap();
        copy_entry(&from, &to, true).unwrap();
        assert!(sandbox.path("plain/SKILL.md").is_file());
        assert!(fs::symlink_metadata(sandbox.path("plain/alias")).is_err());
        assert!(source.join("SKILL.md").is_file(), "复制不动源");
    }

    #[test]
    fn copy_refuses_an_existing_target_a_missing_parent_or_itself() {
        let sandbox = Sandbox::new();
        let host = sandbox.host();
        fs::create_dir_all(sandbox.path("source")).unwrap();
        fs::create_dir_all(sandbox.path("taken")).unwrap();
        assert!(
            host.copy_plan(&sandbox.s("source"), &sandbox.s("taken"))
                .is_err()
        );
        assert!(
            host.copy_plan(&sandbox.s("source"), &sandbox.s("no/parent"))
                .is_err()
        );
        assert!(
            host.copy_plan(&sandbox.s("source"), &sandbox.s("source/inner"))
                .is_err()
        );
        assert!(
            host.copy_plan(&sandbox.s("missing"), &sandbox.s("fresh"))
                .is_err()
        );
    }

    #[test]
    fn copy_options_accept_only_known_symlink_modes() {
        let options = |mode: &str| {
            HostArguments::new([
                HostValue::from("/a"),
                HostValue::from("/b"),
                HostObject::new().field("symlinks", mode).into(),
            ])
        };
        assert!(!skip_symlinks(&HostArguments::new([HostValue::from("/a")])).unwrap());
        assert!(!skip_symlinks(&options("preserve")).unwrap());
        assert!(skip_symlinks(&options("skip")).unwrap());
        assert!(skip_symlinks(&options("follow")).is_err());
    }
}
