//! `~/.smelt` 下的历史残留：顶层没登记过的条目（[`paths::DECLARED_ENTRIES`]）。
//!
//! 只看顶层，条目里面归各自的主人管。不跟软链接走：顶层的软链接只删链接本身，大小也只算
//! 链接。全是阻塞的文件 IO，只在后台线程上调。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use smelt_plugin_api::paths;

/// 一项残留。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Residue {
    pub path: PathBuf,
    /// 顶层的名字，目录带个 `/`。
    pub label: String,
    pub bytes: u64,
}

/// 列出 `~/.smelt` 顶层没登记的条目，大的在前。
pub(crate) fn scan() -> io::Result<Vec<Residue>> {
    let home = paths::smelt_home()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "couldn't find $HOME"))?;
    match scan_in(&home) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        result => result,
    }
}

/// 删掉这几项；返回删不掉的和原因。不是 `~/.smelt` 顶层残留的路径一律不碰。
pub(crate) fn remove(paths: &[PathBuf]) -> Vec<(PathBuf, String)> {
    let Some(home) = paths::smelt_home() else {
        return Vec::new();
    };
    remove_in(&home, paths)
}

fn scan_in(home: &Path) -> io::Result<Vec<Residue>> {
    let mut residue = Vec::new();
    for entry in fs::read_dir(home)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_str().is_some_and(paths::is_declared_entry) {
            continue;
        }
        let path = entry.path();
        let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
        let name = name.to_string_lossy();
        residue.push(Residue {
            label: if is_dir {
                format!("{name}/")
            } else {
                name.into_owned()
            },
            bytes: size_of(&path),
            path,
        });
    }
    residue.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.label.cmp(&b.label)));
    Ok(residue)
}

/// 占的字节数。目录逐层加，不跟软链接；读不到的算 0。
fn size_of(path: &Path) -> u64 {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    fs::read_dir(path)
        .map(|entries| entries.flatten().map(|entry| size_of(&entry.path())).sum())
        .unwrap_or(0)
}

fn remove_in(home: &Path, targets: &[PathBuf]) -> Vec<(PathBuf, String)> {
    let mut failures = Vec::new();
    for path in targets {
        let is_residue = path.parent() == Some(home)
            && path
                .file_name()
                .is_some_and(|name| !name.to_str().is_some_and(paths::is_declared_entry));
        if !is_residue {
            continue;
        }
        if let Err(error) = remove_entry(path) {
            failures.push((path.clone(), error.to_string()));
        }
    }
    failures
}

fn remove_entry(path: &Path) -> io::Result<()> {
    let result = match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) => Err(error),
    };
    match result {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "smelt-residue-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default(),
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn labels(residue: &[Residue]) -> Vec<&str> {
        residue.iter().map(|item| item.label.as_str()).collect()
    }

    #[test]
    fn only_undeclared_top_level_entries_are_residue_biggest_first() {
        let home = scratch("scan");
        fs::write(home.join("smelt.sqlite3"), [0; 8]).unwrap();
        fs::write(home.join("smelt.sqlite3-wal"), [0; 8]).unwrap();
        fs::create_dir_all(home.join("bin")).unwrap();
        fs::write(home.join("bin").join("old-binary"), [0; 64]).unwrap();
        fs::write(home.join("smelt.sqlite3.bak-v38"), [0; 4]).unwrap();
        fs::create_dir_all(home.join("models").join("nested")).unwrap();
        fs::write(home.join("models").join("nested").join("m.gguf"), [0; 32]).unwrap();

        let residue = scan_in(&home).unwrap();

        assert_eq!(labels(&residue), ["models/", "smelt.sqlite3.bak-v38"]);
        assert_eq!(residue[0].bytes, 32, "目录逐层加起来");
        assert_eq!(residue[1].bytes, 4);
        let _ = fs::remove_dir_all(home);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_neither_followed_for_size_nor_on_removal() {
        let home = scratch("link-home");
        let outside = scratch("link-outside");
        fs::write(outside.join("precious"), [0; 1024]).unwrap();
        std::os::unix::fs::symlink(&outside, home.join("linked")).unwrap();

        let residue = scan_in(&home).unwrap();
        assert_eq!(labels(&residue), ["linked"]);
        assert!(residue[0].bytes < 1024);

        assert!(remove_in(&home, &[home.join("linked")]).is_empty());
        assert!(!home.join("linked").exists());
        assert!(
            outside.join("precious").is_file(),
            "链接指向的东西不能跟着删"
        );
        let _ = fs::remove_dir_all(home);
        let _ = fs::remove_dir_all(outside);
    }

    #[test]
    fn removal_deletes_residue_but_refuses_declared_or_foreign_paths() {
        let home = scratch("remove");
        let elsewhere = scratch("remove-elsewhere");
        fs::create_dir_all(home.join("models").join("nested")).unwrap();
        fs::write(home.join("models").join("nested").join("m.gguf"), "x").unwrap();
        fs::write(home.join("smelt.db"), "x").unwrap();
        fs::write(home.join("smelt.sqlite3"), "keep").unwrap();
        fs::create_dir_all(home.join("plugins").join("p")).unwrap();
        fs::write(elsewhere.join("models"), "keep").unwrap();

        let failures = remove_in(
            &home,
            &[
                home.join("models"),
                home.join("smelt.db"),
                home.join("gone-already"),
                home.join("smelt.sqlite3"),
                home.join("plugins").join("p"),
                elsewhere.join("models"),
            ],
        );

        assert!(failures.is_empty(), "{failures:?}");
        assert!(!home.join("models").exists());
        assert!(!home.join("smelt.db").exists());
        assert!(home.join("smelt.sqlite3").is_file());
        assert!(
            home.join("plugins").join("p").is_dir(),
            "条目里面不归这里管"
        );
        assert!(elsewhere.join("models").is_file());
        let _ = fs::remove_dir_all(home);
        let _ = fs::remove_dir_all(elsewhere);
    }
}
