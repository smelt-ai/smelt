//! 路径所在卷的容量。

use std::path::{Path, PathBuf};

use sysinfo::Disks;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    pub mount: PathBuf,
    pub name: String,
    pub file_system: String,
    pub total: u64,
    /// macOS 上和访达一致，含可清除空间（sysinfo 取的是 `AvailableCapacityForImportantUsage`）。
    pub available: u64,
}

/// `path` 所在的卷：挂载点是它前缀的卷里最长的那个。路径不存在、或一个卷都对不上时是 `None`。
pub fn volume_of(path: &Path) -> Option<Volume> {
    let path = resolve(path)?;
    let disks = Disks::new_with_refreshed_list();
    disks
        .list()
        .iter()
        .filter(|disk| path.starts_with(disk.mount_point()))
        .max_by_key(|disk| disk.mount_point().components().count())
        .map(|disk| Volume {
            mount: disk.mount_point().to_path_buf(),
            name: disk.name().to_string_lossy().into_owned(),
            file_system: disk.file_system().to_string_lossy().into_owned(),
            total: disk.total_space(),
            available: disk.available_space(),
        })
}

/// 解开符号链接，链接可能指到别的卷上。Windows 的 `canonicalize` 会加 `\\?\` 前缀，
/// 和挂载点对不上，只取绝对路径。
#[cfg(unix)]
fn resolve(path: &Path) -> Option<PathBuf> {
    path.canonicalize().ok()
}

#[cfg(not(unix))]
fn resolve(path: &Path) -> Option<PathBuf> {
    path.try_exists()
        .ok()
        .filter(|exists| *exists)
        .and_then(|_| std::path::absolute(path).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_dir_lives_on_a_volume_with_space() {
        let dir = tempfile::tempdir().unwrap();
        let volume = volume_of(dir.path()).expect("临时目录所在的卷");
        assert!(
            dir.path()
                .canonicalize()
                .unwrap()
                .starts_with(&volume.mount)
        );
        assert!(volume.total > 0);
        assert!(volume.available <= volume.total);
    }

    #[test]
    fn missing_path_has_no_volume() {
        assert_eq!(volume_of(Path::new("/definitely/not/here")), None);
    }
}
