//! 通用文件系统方法 `fs.*` 的运行时实现：看路径、列目录、读写、建目录、改名复制、删除（可移进
//! 废纸篓）、卷容量、常用位置。线上形状和语义见 `smelt_runtime_client::protocol::fs`。

use std::fs::Metadata;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Component, Path};
use std::time::UNIX_EPOCH;

use base64::Engine as _;
use smelt_runtime_client::protocol::fs::{
    FS_READ_LIMIT, FsDeleteParams, FsDirEntry, FsEncoding, FsIdentity, FsKind, FsKnownDirs,
    FsMoveParams, FsReadParams, FsReadResult, FsStat, FsVolume, FsWriteMode, FsWriteParams,
};

#[derive(Debug, PartialEq, Eq)]
pub enum FsError {
    /// 不是规范的绝对路径（相对路径、带 `..`）。
    InvalidPath(String),
    NotFound(String),
    PermissionDenied(String),
    /// 删除前核对身份没对上。
    Changed(String),
    /// 目标已经存在。
    Exists(String),
    /// 参数本身不成立：读得太多、内容不是合法的 base64 / UTF-8、把目录复制进它自己。
    Invalid(String),
    Io(String),
}

impl std::fmt::Display for FsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPath(path) => write!(f, "{path} isn't an absolute path"),
            Self::NotFound(path) => write!(f, "{path} doesn't exist"),
            Self::PermissionDenied(path) => write!(f, "No permission to access {path}"),
            Self::Changed(path) => write!(f, "{path} has been replaced by something else"),
            Self::Exists(path) => write!(f, "{path} already exists"),
            Self::Invalid(message) | Self::Io(message) => f.write_str(message),
        }
    }
}

fn io_error(path: &str, error: std::io::Error) -> FsError {
    match error.kind() {
        std::io::ErrorKind::NotFound => FsError::NotFound(path.to_string()),
        std::io::ErrorKind::PermissionDenied => FsError::PermissionDenied(path.to_string()),
        std::io::ErrorKind::AlreadyExists => FsError::Exists(path.to_string()),
        _ => FsError::Io(format!("{path}: {error}")),
    }
}

/// 只收规范的绝对路径：删除这种操作，`..` 拼出来的路径不该靠运行时去猜。
fn require_absolute(path: &str) -> Result<&Path, FsError> {
    let p = Path::new(path);
    if p.is_absolute() && !p.components().any(|c| matches!(c, Component::ParentDir)) {
        Ok(p)
    } else {
        Err(FsError::InvalidPath(path.to_string()))
    }
}

pub fn kind_of(meta: &Metadata) -> FsKind {
    let file_type = meta.file_type();
    if file_type.is_symlink() {
        FsKind::Symlink
    } else if file_type.is_dir() {
        FsKind::Dir
    } else if file_type.is_file() {
        FsKind::File
    } else {
        FsKind::Other
    }
}

/// 实际占的磁盘空间。Unix 按块数算；别的平台拿不到，退回名义长度。
#[cfg(unix)]
pub fn allocated_of(meta: &Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    meta.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
pub fn allocated_of(meta: &Metadata) -> u64 {
    meta.len()
}

#[cfg(unix)]
pub fn identity_of(meta: &Metadata) -> Option<FsIdentity> {
    use std::os::unix::fs::MetadataExt as _;
    Some(FsIdentity {
        dev: meta.dev(),
        ino: meta.ino(),
    })
}

#[cfg(not(unix))]
pub fn identity_of(_meta: &Metadata) -> Option<FsIdentity> {
    None
}

pub fn modified_of(meta: &Metadata) -> Option<i64> {
    let since = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(since.as_millis()).ok()
}

/// `meta` 是 `path` 不跟随链接的元数据。链接再跟一次，看它指向什么。
fn stat_from(path: &Path, meta: &Metadata) -> FsStat {
    let kind = kind_of(meta);
    let target_kind = (kind == FsKind::Symlink)
        .then(|| std::fs::metadata(path).ok().map(|target| kind_of(&target)))
        .flatten();
    FsStat {
        kind,
        target_kind,
        size: meta.len(),
        allocated: allocated_of(meta),
        modified: modified_of(meta),
        identity: identity_of(meta),
    }
}

pub fn stat(path: &str) -> Result<FsStat, FsError> {
    let p = require_absolute(path)?;
    let meta = std::fs::symlink_metadata(p).map_err(|e| io_error(path, e))?;
    Ok(stat_from(p, &meta))
}

pub fn read_dir(path: &str) -> Result<Vec<FsDirEntry>, FsError> {
    let p = require_absolute(path)?;
    let entries = std::fs::read_dir(p).map_err(|e| io_error(path, e))?;
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| io_error(path, e))?;
        // 列的过程中被删掉的子项拿不到元数据，跳过。
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        out.push(FsDirEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            stat: stat_from(&entry.path(), &meta),
        });
    }
    Ok(out)
}

pub fn delete(params: &FsDeleteParams) -> Result<(), FsError> {
    let path = params.path.as_str();
    let p = require_absolute(path)?;
    if p.parent().is_none() {
        return Err(FsError::InvalidPath(path.to_string()));
    }
    let meta = std::fs::symlink_metadata(p).map_err(|e| io_error(path, e))?;
    if let Some(expect) = params.expect
        && identity_of(&meta) != Some(expect)
    {
        return Err(FsError::Changed(path.to_string()));
    }
    if params.trash {
        return move_to_trash(p).map_err(|error| FsError::Io(format!("{path}: {error}")));
    }
    let result = if meta.is_dir() {
        std::fs::remove_dir_all(p)
    } else {
        std::fs::remove_file(p)
    };
    result.map_err(|e| io_error(path, e))
}

/// macOS 上走 `NSFileManager`：运行时是后台进程，走访达（osascript）要先拿到控制访达的
/// 自动化授权，还会响一声。代价是访达里没有「放回原处」。
fn move_to_trash(path: &Path) -> Result<(), trash::Error> {
    #[allow(unused_mut)]
    let mut context = trash::TrashContext::default();
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod, TrashContextExtMacos as _};
        context.set_delete_method(DeleteMethod::NsFileManager);
    }
    context.delete(path)
}

pub fn known_dirs() -> Result<FsKnownDirs, FsError> {
    let home =
        dirs::home_dir().ok_or_else(|| FsError::Io("couldn't find the home directory".into()))?;
    let string = |path: std::path::PathBuf| path.to_string_lossy().into_owned();
    let trash = if cfg!(target_os = "macos") {
        Some(home.join(".Trash"))
    } else if cfg!(windows) {
        None
    } else {
        dirs::data_dir().map(|data| data.join("Trash"))
    };
    Ok(FsKnownDirs {
        cache: dirs::cache_dir().map(string),
        config: dirs::config_dir().map(string),
        data: dirs::data_dir().map(string),
        temp: string(std::env::temp_dir()),
        desktop: dirs::desktop_dir().map(string),
        documents: dirs::document_dir().map(string),
        downloads: dirs::download_dir().map(string),
        trash: trash.map(string),
        home: string(home),
    })
}

pub fn create_dir(path: &str) -> Result<(), FsError> {
    let p = require_absolute(path)?;
    match std::fs::create_dir_all(p) {
        Ok(()) => Ok(()),
        // 上级或自己是个文件。
        Err(_) if p.exists() && !p.is_dir() => Err(FsError::Exists(path.to_string())),
        Err(error) => Err(io_error(path, error)),
    }
}

/// 目标和来源是不是同一个东西。大小写不敏感的卷上，只改大小写的两个路径就是同一个。
fn same_entry(from: &Path, to: &Metadata) -> bool {
    std::fs::symlink_metadata(from)
        .ok()
        .and_then(|source| identity_of(&source))
        .is_some_and(|source| Some(source) == identity_of(to))
}

/// 改名、复制前腾出目标：不存在就好；存在又不许覆盖就报错；许覆盖就删掉。`keep_files` 时
/// 目标是文件就不删。
fn clear_target(to: &Path, params: &FsMoveParams, keep_files: bool) -> Result<(), FsError> {
    let Ok(meta) = std::fs::symlink_metadata(to) else {
        return Ok(());
    };
    if !params.overwrite {
        return Err(FsError::Exists(params.to.clone()));
    }
    let removed = if meta.is_dir() {
        std::fs::remove_dir_all(to)
    } else if keep_files {
        return Ok(());
    } else {
        std::fs::remove_file(to)
    };
    removed.map_err(|e| io_error(&params.to, e))
}

fn move_paths(params: &FsMoveParams) -> Result<(&Path, &Path), FsError> {
    let from = require_absolute(&params.from)?;
    let to = require_absolute(&params.to)?;
    for (path, raw) in [(from, &params.from), (to, &params.to)] {
        if path.parent().is_none() {
            return Err(FsError::InvalidPath(raw.clone()));
        }
    }
    std::fs::symlink_metadata(from).map_err(|e| io_error(&params.from, e))?;
    if to != from && to.starts_with(from) {
        return Err(FsError::Invalid(format!(
            "can't put {} inside itself",
            params.from
        )));
    }
    Ok((from, to))
}

pub fn rename(params: &FsMoveParams) -> Result<(), FsError> {
    let (from, to) = move_paths(params)?;
    let same = std::fs::symlink_metadata(to).is_ok_and(|meta| same_entry(from, &meta));
    if !same {
        // 文件换文件交给 rename 原子替换；目录和文件互换 rename 做不了，先删目标。
        let from_is_dir = std::fs::symlink_metadata(from).is_ok_and(|meta| meta.is_dir());
        clear_target(to, params, !from_is_dir)?;
    }
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {
            clear_target(to, params, false)?;
            copy_tree(from, to).map_err(|e| io_error(&params.to, e))?;
            let meta = std::fs::symlink_metadata(from).map_err(|e| io_error(&params.from, e))?;
            let removed = if meta.is_dir() {
                std::fs::remove_dir_all(from)
            } else {
                std::fs::remove_file(from)
            };
            removed.map_err(|e| io_error(&params.from, e))
        }
        Err(error) => Err(io_error(&params.from, error)),
    }
}

pub fn copy(params: &FsMoveParams) -> Result<(), FsError> {
    let (from, to) = move_paths(params)?;
    if from == to || std::fs::symlink_metadata(to).is_ok_and(|meta| same_entry(from, &meta)) {
        return Err(FsError::Exists(params.to.clone()));
    }
    clear_target(to, params, false)?;
    copy_tree(from, to).map_err(|e| io_error(&params.to, e))
}

/// 复制一个文件、符号链接或整个目录。目标不能已经存在。macOS 上 `std::fs::copy` 在 APFS
/// 上是克隆，不占新空间。
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(from)?;
    if meta.file_type().is_symlink() {
        return copy_symlink(from, to);
    }
    if !meta.is_dir() {
        return std::fs::copy(from, to).map(|_| ());
    }
    std::fs::create_dir(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        copy_tree(&entry.path(), &to.join(entry.file_name()))?;
    }
    std::fs::set_permissions(to, meta.permissions())
}

#[cfg(unix)]
fn copy_symlink(from: &Path, to: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(std::fs::read_link(from)?, to)
}

#[cfg(windows)]
fn copy_symlink(from: &Path, to: &Path) -> std::io::Result<()> {
    let target = std::fs::read_link(from)?;
    if std::fs::metadata(from).is_ok_and(|meta| meta.is_dir()) {
        std::os::windows::fs::symlink_dir(target, to)
    } else {
        std::os::windows::fs::symlink_file(target, to)
    }
}

#[cfg(not(any(unix, windows)))]
fn copy_symlink(_from: &Path, _to: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "can't copy symbolic links on this platform",
    ))
}

pub fn read(params: &FsReadParams) -> Result<FsReadResult, FsError> {
    let path = params.path.as_str();
    let p = require_absolute(path)?;
    let mut file = std::fs::File::open(p).map_err(|e| io_error(path, e))?;
    let size = file.metadata().map_err(|e| io_error(path, e))?.len();
    let available = size.saturating_sub(params.offset);
    let length = match params.length {
        Some(length) => length,
        None => available,
    };
    if length > FS_READ_LIMIT {
        return Err(FsError::Invalid(format!(
            "{path}: can't read more than {FS_READ_LIMIT} bytes at once; read it in ranges"
        )));
    }
    let mut bytes = Vec::with_capacity(length.min(available) as usize);
    file.seek(SeekFrom::Start(params.offset))
        .map_err(|e| io_error(path, e))?;
    file.take(length)
        .read_to_end(&mut bytes)
        .map_err(|e| io_error(path, e))?;
    let content = match params.encoding {
        FsEncoding::Base64 => base64::engine::general_purpose::STANDARD.encode(&bytes),
        FsEncoding::Utf8 => String::from_utf8(bytes)
            .map_err(|_| FsError::Invalid(format!("{path} isn't UTF-8 text")))?,
    };
    Ok(FsReadResult { size, content })
}

pub fn write(params: &FsWriteParams) -> Result<(), FsError> {
    let path = params.path.as_str();
    let p = require_absolute(path)?;
    let bytes = match params.encoding {
        FsEncoding::Base64 => base64::engine::general_purpose::STANDARD
            .decode(&params.content)
            .map_err(|_| FsError::Invalid("content isn't valid base64".into()))?,
        FsEncoding::Utf8 => params.content.clone().into_bytes(),
    };
    let mut options = std::fs::OpenOptions::new();
    match params.mode {
        FsWriteMode::Overwrite => options.write(true).create(true).truncate(true),
        FsWriteMode::Create => options.write(true).create_new(true),
        FsWriteMode::Append => options.append(true).create(true),
    };
    let mut file = options.open(p).map_err(|e| io_error(path, e))?;
    file.write_all(&bytes).map_err(|e| io_error(path, e))
}

pub fn volume(path: &str) -> Result<FsVolume, FsError> {
    let p = require_absolute(path)?;
    let volume = smelt_sys::volume::volume_of(p).ok_or_else(|| FsError::NotFound(path.into()))?;
    Ok(FsVolume {
        mount: volume.mount.to_string_lossy().into_owned(),
        name: volume.name,
        file_system: volume.file_system,
        total: volume.total,
        available: volume.available,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "smelt-fs-ops-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn s(path: &Path) -> &str {
        path.to_str().unwrap()
    }

    #[test]
    fn stat_reports_kind_size_and_link_target() {
        let root = temp_dir("stat");
        std::fs::write(root.join("a.txt"), "hello").unwrap();
        std::fs::create_dir(root.join("dir")).unwrap();

        let file = stat(s(&root.join("a.txt"))).unwrap();
        assert_eq!(file.kind, FsKind::File);
        assert_eq!(file.size, 5);
        assert_eq!(file.target_kind, None);
        assert!(file.modified.is_some());

        assert_eq!(stat(s(&root.join("dir"))).unwrap().kind, FsKind::Dir);
        assert_eq!(
            stat(s(&root.join("missing"))),
            Err(FsError::NotFound(s(&root.join("missing")).into()))
        );

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("dir"), root.join("link")).unwrap();
            let link = stat(s(&root.join("link"))).unwrap();
            assert_eq!(link.kind, FsKind::Symlink);
            assert_eq!(link.target_kind, Some(FsKind::Dir));
            assert!(file.identity.is_some());
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn relative_and_dotdot_paths_are_rejected() {
        assert!(matches!(stat("a/b"), Err(FsError::InvalidPath(_))));
        assert!(matches!(
            read_dir("/tmp/../etc"),
            Err(FsError::InvalidPath(_))
        ));
        let root = FsDeleteParams {
            path: "/".into(),
            ..Default::default()
        };
        assert!(matches!(delete(&root), Err(FsError::InvalidPath(_))));
    }

    #[test]
    fn read_dir_lists_everything_unsorted_and_reports_missing_dirs() {
        let root = temp_dir("read-dir");
        std::fs::create_dir(root.join(".git")).unwrap();
        std::fs::write(root.join("b.txt"), "b").unwrap();
        std::fs::create_dir(root.join("a")).unwrap();

        let mut names: Vec<_> = read_dir(s(&root))
            .unwrap()
            .into_iter()
            .map(|entry| (entry.name, entry.stat.kind))
            .collect();
        names.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            names,
            vec![
                (".git".to_string(), FsKind::Dir),
                ("a".to_string(), FsKind::Dir),
                ("b.txt".to_string(), FsKind::File),
            ]
        );
        assert!(matches!(
            read_dir(s(&root.join("missing"))),
            Err(FsError::NotFound(_))
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn delete_removes_files_dirs_and_only_the_link_itself() {
        let root = temp_dir("delete");
        std::fs::create_dir_all(root.join("dir/nested")).unwrap();
        std::fs::write(root.join("dir/nested/a.txt"), "a").unwrap();
        std::fs::write(root.join("b.txt"), "b").unwrap();
        let del = |path: &Path| {
            delete(&FsDeleteParams {
                path: s(path).into(),
                ..Default::default()
            })
        };

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("dir"), root.join("link")).unwrap();
            del(&root.join("link")).unwrap();
            assert!(!root.join("link").exists());
            assert!(root.join("dir/nested/a.txt").exists());
        }
        del(&root.join("b.txt")).unwrap();
        del(&root.join("dir")).unwrap();
        assert!(!root.join("b.txt").exists());
        assert!(!root.join("dir").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn delete_refuses_when_the_path_now_points_elsewhere() {
        let root = temp_dir("expect");
        let path = root.join("a.txt");
        std::fs::write(&path, "old").unwrap();
        let seen = stat(s(&path)).unwrap().identity;
        // 新文件在旧文件还在时建好再改名盖过去，两个同时存在，inode 不可能相同。
        std::fs::write(root.join("other"), "new").unwrap();
        std::fs::rename(root.join("other"), &path).unwrap();

        let result = delete(&FsDeleteParams {
            path: s(&path).into(),
            trash: false,
            expect: seen,
        });
        assert_eq!(result, Err(FsError::Changed(s(&path).into())));
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn volume_of_temp_dir_has_capacity() {
        let root = temp_dir("volume");
        let volume = volume(s(&root)).unwrap();
        assert!(volume.total > 0);
        assert!(volume.available <= volume.total);
        let _ = std::fs::remove_dir_all(root);
    }

    fn moving(from: &Path, to: &Path, overwrite: bool) -> FsMoveParams {
        FsMoveParams {
            from: s(from).into(),
            to: s(to).into(),
            overwrite,
        }
    }

    fn writing(path: &Path, text: &str, mode: FsWriteMode) -> FsWriteParams {
        FsWriteParams {
            path: s(path).into(),
            content: text.into(),
            encoding: FsEncoding::Utf8,
            mode,
        }
    }

    #[test]
    fn write_modes_create_replace_and_append() {
        let root = temp_dir("write");
        let path = root.join("a.txt");
        write(&writing(&path, "one", FsWriteMode::Create)).unwrap();
        assert_eq!(
            write(&writing(&path, "two", FsWriteMode::Create)),
            Err(FsError::Exists(s(&path).into()))
        );
        write(&writing(&path, "two", FsWriteMode::Overwrite)).unwrap();
        write(&writing(&path, "+", FsWriteMode::Append)).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two+");

        let bytes = FsWriteParams {
            path: s(&root.join("b.bin")).into(),
            content: "AP8=".into(),
            encoding: FsEncoding::Base64,
            mode: FsWriteMode::Overwrite,
        };
        write(&bytes).unwrap();
        assert_eq!(std::fs::read(root.join("b.bin")).unwrap(), [0x00, 0xff]);

        // 不建上级目录。
        let nested = root.join("missing/c.txt");
        assert_eq!(
            write(&writing(&nested, "x", FsWriteMode::Overwrite)),
            Err(FsError::NotFound(s(&nested).into()))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn read_gives_ranges_in_either_encoding_and_refuses_too_much() {
        let root = temp_dir("read");
        let path = root.join("a.txt");
        std::fs::write(&path, "hello world").unwrap();
        let read_as = |offset, length, encoding| {
            read(&FsReadParams {
                path: s(&path).into(),
                offset,
                length,
                encoding,
            })
        };
        assert_eq!(
            read_as(0, None, FsEncoding::Utf8).unwrap(),
            FsReadResult {
                size: 11,
                content: "hello world".into()
            }
        );
        assert_eq!(
            read_as(6, Some(3), FsEncoding::Utf8).unwrap().content,
            "wor"
        );
        assert_eq!(
            read_as(0, Some(2), FsEncoding::Base64).unwrap().content,
            "aGU="
        );
        // 读过了末尾是空的，不报错。
        assert_eq!(read_as(50, None, FsEncoding::Utf8).unwrap().content, "");
        assert!(matches!(
            read_as(0, Some(FS_READ_LIMIT + 1), FsEncoding::Base64),
            Err(FsError::Invalid(_))
        ));

        std::fs::write(&path, [0xff, 0xfe]).unwrap();
        assert!(matches!(
            read_as(0, None, FsEncoding::Utf8),
            Err(FsError::Invalid(_))
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn create_dir_makes_parents_and_rejects_a_file_in_the_way() {
        let root = temp_dir("mkdir");
        let nested = root.join("a/b/c");
        create_dir(s(&nested)).unwrap();
        create_dir(s(&nested)).unwrap();
        assert!(nested.is_dir());

        let file = root.join("file");
        std::fs::write(&file, "x").unwrap();
        assert_eq!(create_dir(s(&file)), Err(FsError::Exists(s(&file).into())));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rename_and_copy_respect_overwrite() {
        let root = temp_dir("move");
        let a = root.join("a.txt");
        let b = root.join("b.txt");
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();

        assert_eq!(
            rename(&moving(&a, &b, false)),
            Err(FsError::Exists(s(&b).into()))
        );
        assert_eq!(
            copy(&moving(&a, &b, false)),
            Err(FsError::Exists(s(&b).into()))
        );
        copy(&moving(&a, &b, true)).unwrap();
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "a");

        std::fs::write(&b, "b").unwrap();
        rename(&moving(&a, &b, true)).unwrap();
        assert!(!a.exists());
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "a");

        // 目录可以覆盖文件。
        let dir = root.join("dir");
        std::fs::create_dir(&dir).unwrap();
        rename(&moving(&dir, &b, true)).unwrap();
        assert!(b.is_dir());

        assert_eq!(
            copy(&moving(&b, &b, true)),
            Err(FsError::Exists(s(&b).into()))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// 大小写不敏感的卷上，「目标」就是来源本身：不能当成已存在，更不能先删掉。
    #[test]
    fn renaming_only_the_case_keeps_the_file() {
        let root = temp_dir("case");
        let lower = root.join("a.txt");
        std::fs::write(&lower, "a").unwrap();
        let upper = root.join("A.txt");
        if !upper.exists() {
            // 大小写敏感的卷，没有这个问题。
            let _ = std::fs::remove_dir_all(root);
            return;
        }
        rename(&moving(&lower, &upper, false)).unwrap();
        let names: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["A.txt"]);
        assert_eq!(std::fs::read_to_string(&upper).unwrap(), "a");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_directory_cant_go_inside_itself() {
        let root = temp_dir("inside");
        let dir = root.join("dir");
        std::fs::create_dir(&dir).unwrap();
        let inner = dir.join("inner");
        assert!(matches!(
            rename(&moving(&dir, &inner, false)),
            Err(FsError::Invalid(_))
        ));
        assert!(matches!(
            copy(&moving(&dir, &inner, false)),
            Err(FsError::Invalid(_))
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn copy_duplicates_a_tree_and_keeps_links_as_links() {
        let root = temp_dir("copy-tree");
        let from = root.join("from");
        std::fs::create_dir_all(from.join("sub")).unwrap();
        std::fs::write(from.join("sub/a.txt"), "a").unwrap();
        std::os::unix::fs::symlink("sub/a.txt", from.join("link")).unwrap();

        let to = root.join("to");
        copy(&moving(&from, &to, false)).unwrap();
        assert_eq!(std::fs::read_to_string(to.join("sub/a.txt")).unwrap(), "a");
        assert_eq!(
            std::fs::read_link(to.join("link")).unwrap(),
            Path::new("sub/a.txt")
        );
        assert!(from.join("sub/a.txt").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn known_dirs_are_absolute() {
        let dirs = known_dirs().unwrap();
        assert!(Path::new(&dirs.home).is_absolute());
        assert!(Path::new(&dirs.temp).is_absolute());
        if cfg!(target_os = "macos") {
            assert_eq!(dirs.trash, Some(format!("{}/.Trash", dirs.home)));
            assert_eq!(dirs.cache, Some(format!("{}/Library/Caches", dirs.home)));
        }
    }
}
