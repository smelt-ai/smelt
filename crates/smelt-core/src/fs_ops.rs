//! 通用文件系统方法 `fs.*` 的运行时实现：看路径、列目录、删除（可移进废纸篓）、卷容量。
//! 线上形状和语义见 `smelt_runtime_client::protocol::fs`。

use std::fs::Metadata;
use std::path::{Component, Path};
use std::time::UNIX_EPOCH;

use smelt_runtime_client::protocol::fs::{
    FsDeleteParams, FsDirEntry, FsIdentity, FsKind, FsStat, FsVolume,
};

#[derive(Debug, PartialEq, Eq)]
pub enum FsError {
    /// 不是规范的绝对路径（相对路径、带 `..`）。
    InvalidPath(String),
    NotFound(String),
    PermissionDenied(String),
    /// 删除前核对身份没对上。
    Changed(String),
    Io(String),
}

impl std::fmt::Display for FsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPath(path) => write!(f, "{path} isn't an absolute path"),
            Self::NotFound(path) => write!(f, "{path} doesn't exist"),
            Self::PermissionDenied(path) => write!(f, "No permission to access {path}"),
            Self::Changed(path) => write!(f, "{path} has been replaced by something else"),
            Self::Io(message) => f.write_str(message),
        }
    }
}

fn io_error(path: &str, error: std::io::Error) -> FsError {
    match error.kind() {
        std::io::ErrorKind::NotFound => FsError::NotFound(path.to_string()),
        std::io::ErrorKind::PermissionDenied => FsError::PermissionDenied(path.to_string()),
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
}
