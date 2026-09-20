//! 文件系统能力接缝（Service Definition + Provider + Consumer）。
//!
//! **为什么要这层间接**：smelt 现在有 600+ 处直接 `std::fs::`，等价于把「文件
//! 住在本机」这条假设焊死在每个调用点上。远程访问（iroh）、worktree 沙箱、
//! 以及将来自研 agent 的模型可见文件工具，都需要同一套读写换一个执行世界；
//! 没有这层，每加一个世界就得再抄一遍所有调用点。
//!
//! 三个角色缺一不可，只有接口没有第二个实现不算接缝：
//! - **Definition**：[`FileSystem`]，能力面故意收得很窄（读/写/列/删/存在性），
//!   够 UI 的文件树与编辑器用，也够模型可见的文件工具用。
//! - **Provider**：[`LocalFs`]（本机真盘）；测试中另有内存实现 `MemFs`，作为
//!   远程/沙箱实现的参照。
//! - **Consumer**：[`list_dir`] 这类只依赖 trait 的纯逻辑，换 provider 即可
//!   在别的执行世界里原样复用。
//!
//! 阻塞式而非 async：现有调用方都已经在后台线程/`background_executor` 里跑，
//! 引入 async trait 只会逼所有同步调用点改造，收益为零。

#[cfg(test)]
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::{Arc, Mutex};

/// 一个目录项。故意不暴露 `std::fs::DirEntry`——那是本地实现细节，远程
/// provider 造不出来。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

/// 文件系统能力。实现者负责把路径语义映射到自己的执行世界。
///
/// 错误一律用 [`io::Error`]：本地实现直接透传，远程实现把协议错误翻译过来，
/// 调用方不需要认识具体 provider 的错误类型。
pub trait FileSystem: Send + Sync {
    fn read_to_string(&self, path: &Path) -> io::Result<String>;

    fn write(&self, path: &Path, contents: &str) -> io::Result<()>;

    /// 列一层目录（不递归）。顺序由调用方决定，见 [`list_dir`]。
    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>>;

    fn remove_file(&self, path: &Path) -> io::Result<()>;

    fn remove_dir_all(&self, path: &Path) -> io::Result<()>;

    /// 创建单层目录。父目录必须已存在，目标已存在返回 `AlreadyExists`。
    /// 用于需要独占创建语义的场景（如暂存区竞争）。
    fn create_dir(&self, path: &Path) -> io::Result<()>;

    fn create_dir_all(&self, path: &Path) -> io::Result<()>;

    /// 重命名/移动文件或目录。
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;

    /// 复制文件（不递归复制目录）。
    fn copy(&self, from: &Path, to: &Path) -> io::Result<()>;

    /// 返回规范化的绝对路径，解析所有符号链接和 `.`/`..`。
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf>;

    /// 路径是否存在，以及是不是目录。不存在返回 `None`。
    /// 跟随符号链接。
    fn metadata(&self, path: &Path) -> Option<Metadata>;

    /// 不跟随符号链接的元数据查询。用于检测符号链接本身。
    fn symlink_metadata(&self, path: &Path) -> Option<Metadata>;

    fn exists(&self, path: &Path) -> bool {
        self.metadata(path).is_some()
    }

    fn is_dir(&self, path: &Path) -> bool {
        self.metadata(path).is_some_and(|meta| meta.is_dir)
    }

    fn is_file(&self, path: &Path) -> bool {
        self.metadata(path).is_some_and(|meta| !meta.is_dir)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metadata {
    pub is_dir: bool,
    pub is_symlink: bool,
    pub len: u64,
}

// ===================== Provider：本机真盘 =====================

/// 本机文件系统。语义即 `std::fs`，是所有非远程场景的默认 provider。
#[derive(Clone, Copy, Debug, Default)]
pub struct LocalFs;

impl FileSystem for LocalFs {
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        std::fs::read_to_string(path)
    }

    fn write(&self, path: &Path, contents: &str) -> io::Result<()> {
        std::fs::write(path, contents)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(path)?.flatten() {
            let entry_path = entry.path();
            out.push(DirEntry {
                name: entry.file_name().to_string_lossy().to_string(),
                path: entry_path.clone(),
                // 用 entry.path().is_dir() 而不是 file_type()：跟随符号链接，
                // 与被替换的 file_tree 原逻辑保持一致（skills 目录里全是
                // 指向真身的 symlink，按 file_type 会全被判成非目录）。
                is_dir: entry_path.is_dir(),
            });
        }
        Ok(out)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_file(path)
    }

    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_dir_all(path)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        std::fs::create_dir(path)
    }

    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        std::fs::create_dir_all(path)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }

    fn copy(&self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::copy(from, to)?;
        Ok(())
    }

    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        std::fs::canonicalize(path)
    }

    fn metadata(&self, path: &Path) -> Option<Metadata> {
        let meta = std::fs::metadata(path).ok()?;
        Some(Metadata {
            is_dir: meta.is_dir(),
            is_symlink: false, // metadata() 跟随链接，所以这里始终 false
            len: meta.len(),
        })
    }

    fn symlink_metadata(&self, path: &Path) -> Option<Metadata> {
        let meta = std::fs::symlink_metadata(path).ok()?;
        Some(Metadata {
            is_dir: meta.is_dir(),
            is_symlink: meta.is_symlink(),
            len: meta.len(),
        })
    }
}

// ===================== Provider：内存 =====================

/// 内存文件系统。两个用途：单测不碰真盘；以及作为「第二个实现」证明
/// [`FileSystem`] 真的是接缝而不是只为本地写的接口——只有一个实现的 trait
/// 会在不知不觉中长出本地专属假设。
#[cfg(test)]
#[derive(Clone, Default)]
pub struct MemFs {
    /// path -> 内容。目录用 `None` 表示（同一张表保证「同路径不能既是文件
    /// 又是目录」这条不变量只在一处维护）。
    inner: Arc<Mutex<BTreeMap<PathBuf, Option<String>>>>,
}

#[cfg(test)]
impl MemFs {
    pub fn new() -> Self {
        Self::default()
    }

    /// 测试用：一次性铺好若干文件（父目录自动创建）。
    pub fn with_files<'a>(files: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let fs = Self::new();
        for (path, contents) in files {
            let path = PathBuf::from(path);
            if let Some(parent) = path.parent() {
                let _ = fs.create_dir_all(parent);
            }
            let _ = fs.write(&path, contents);
        }
        fs
    }

    fn not_found(path: &Path) -> io::Error {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} 不存在", path.display()),
        )
    }
}

#[cfg(test)]
impl FileSystem for MemFs {
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        match self.inner.lock().unwrap().get(path) {
            Some(Some(contents)) => Ok(contents.clone()),
            Some(None) => Err(io::Error::new(
                io::ErrorKind::IsADirectory,
                format!("{} 是目录", path.display()),
            )),
            None => Err(Self::not_found(path)),
        }
    }

    fn write(&self, path: &Path, contents: &str) -> io::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        if matches!(inner.get(path), Some(None)) {
            return Err(io::Error::new(
                io::ErrorKind::IsADirectory,
                format!("{} 是目录", path.display()),
            ));
        }
        inner.insert(path.to_path_buf(), Some(contents.to_string()));
        Ok(())
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        let inner = self.inner.lock().unwrap();
        if !matches!(inner.get(path), Some(None)) {
            return Err(Self::not_found(path));
        }
        let mut out = Vec::new();
        for (candidate, payload) in inner.iter() {
            if candidate.parent() != Some(path) {
                continue;
            }
            let Some(name) = candidate.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            out.push(DirEntry {
                name: name.to_string(),
                path: candidate.clone(),
                is_dir: payload.is_none(),
            });
        }
        Ok(out)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        match inner.get(path) {
            Some(Some(_)) => {
                inner.remove(path);
                Ok(())
            }
            Some(None) => Err(io::Error::new(
                io::ErrorKind::IsADirectory,
                format!("{} 是目录", path.display()),
            )),
            None => Err(Self::not_found(path)),
        }
    }

    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        if !matches!(inner.get(path), Some(None)) {
            return Err(Self::not_found(path));
        }
        inner.retain(|candidate, _| candidate != path && !candidate.starts_with(path));
        Ok(())
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        // 检查父目录是否存在且是目录
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
            && !matches!(inner.get(parent), Some(None))
        {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("父目录 {} 不存在", parent.display()),
            ));
        }
        // 检查目标是否已存在
        if inner.contains_key(path) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} 已存在", path.display()),
            ));
        }
        inner.insert(path.to_path_buf(), None);
        Ok(())
    }

    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        for ancestor in path.ancestors() {
            if ancestor.as_os_str().is_empty() {
                continue;
            }
            match inner.get(ancestor) {
                Some(Some(_)) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!("{} 已是文件", ancestor.display()),
                    ));
                }
                Some(None) => {}
                None => {
                    inner.insert(ancestor.to_path_buf(), None);
                }
            }
        }
        Ok(())
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let content = inner.remove(from).ok_or_else(|| Self::not_found(from))?;
        // 如果是目录，还需要搬移所有子路径
        if content.is_none() {
            // 收集所有以 from 为前缀的路径
            let children: Vec<_> = inner
                .keys()
                .filter(|p| p.starts_with(from))
                .cloned()
                .collect();
            for old_path in children {
                if let Ok(rel) = old_path.strip_prefix(from) {
                    let new_path = to.join(rel);
                    if let Some(child_content) = inner.remove(&old_path) {
                        inner.insert(new_path, child_content);
                    }
                }
            }
        }
        inner.insert(to.to_path_buf(), content);
        Ok(())
    }

    fn copy(&self, from: &Path, to: &Path) -> io::Result<()> {
        let inner = self.inner.lock().unwrap();
        let content = inner
            .get(from)
            .ok_or_else(|| Self::not_found(from))?
            .clone()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::IsADirectory,
                    format!("{} 是目录，不能复制", from.display()),
                )
            })?;
        drop(inner);
        let mut inner = self.inner.lock().unwrap();
        inner.insert(to.to_path_buf(), Some(content));
        Ok(())
    }

    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        // MemFs 不支持真正的路径规范化，简单返回原路径
        // （符号链接等在 MemFs 里没有意义）
        if self.inner.lock().unwrap().contains_key(path) {
            Ok(path.to_path_buf())
        } else {
            Err(Self::not_found(path))
        }
    }

    fn metadata(&self, path: &Path) -> Option<Metadata> {
        self.inner
            .lock()
            .unwrap()
            .get(path)
            .map(|payload| match payload {
                Some(contents) => Metadata {
                    is_dir: false,
                    is_symlink: false,
                    len: contents.len() as u64,
                },
                None => Metadata {
                    is_dir: true,
                    is_symlink: false,
                    len: 0,
                },
            })
    }

    fn symlink_metadata(&self, path: &Path) -> Option<Metadata> {
        // MemFs 不支持符号链接，行为与 metadata 相同
        self.metadata(path)
    }
}

// ===================== Consumer：只依赖 trait 的纯逻辑 =====================

/// 文件树默认隐藏的目录项。噪音目录不该由每个调用点各写一份。
pub const HIDDEN_ENTRIES: [&str; 4] = [".git", "node_modules", "target", ".DS_Store"];

/// 列目录给文件树用：目录在前、同类按名字不区分大小写排序，并滤掉
/// [`HIDDEN_ENTRIES`]。读不到目录时返回空列表（文件树原语义：不弹错、当空目录）。
///
/// 这是 Consumer 的样板——**只认 [`FileSystem`]，不认 `std::fs`**，所以本地
/// 面板和将来的远程 worktree 用的是同一份排序与过滤规则，不会各自漂移。
pub fn list_dir(fs: &dyn FileSystem, dir: &Path) -> Vec<DirEntry> {
    let mut items = match fs.read_dir(dir) {
        Ok(items) => items,
        Err(_) => return Vec::new(),
    };
    items.retain(|entry| !HIDDEN_ENTRIES.contains(&entry.name.as_str()));
    items.sort_by_key(|entry| (!entry.is_dir, entry.name.to_lowercase()));
    items
}

/// 检查路径是否存在且是普通文件（非目录）。
///
/// 这是 Consumer 的样板——**只认 [`FileSystem`]，不认 `std::fs`**。
pub fn is_regular_file(fs: &dyn FileSystem, path: &Path) -> bool {
    fs.is_file(path)
}

/// 检查路径是否存在且是符号链接。
///
/// 这是 Consumer 的样板——**只认 [`FileSystem`]，不认 `std::fs`**。
pub fn is_symlink(fs: &dyn FileSystem, path: &Path) -> bool {
    fs.symlink_metadata(path).is_some_and(|m| m.is_symlink)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 同一个 Consumer 跑在两个 Provider 上必须给出一致结果——这是「它确实是
    /// 接缝」的判据，只有一个实现的 trait 证明不了任何事。
    fn assert_list_dir_contract(fs: &dyn FileSystem, root: &Path) {
        let listed = list_dir(fs, root);
        let names: Vec<_> = listed.iter().map(|e| e.name.as_str()).collect();

        assert_eq!(
            names,
            vec!["src", "zeta", "Alpha.md", "beta.rs"],
            "目录在前，其后按名字不区分大小写排序"
        );
        assert!(listed[0].is_dir && listed[1].is_dir);
        assert!(!listed[2].is_dir && !listed[3].is_dir);
    }

    fn seed_layout(fs: &dyn FileSystem, root: &Path) {
        fs.create_dir_all(&root.join("src")).unwrap();
        fs.create_dir_all(&root.join("zeta")).unwrap();
        fs.create_dir_all(&root.join("target")).unwrap();
        fs.create_dir_all(&root.join(".git")).unwrap();
        fs.write(&root.join("beta.rs"), "fn main() {}").unwrap();
        fs.write(&root.join("Alpha.md"), "# alpha").unwrap();
        fs.write(&root.join(".DS_Store"), "junk").unwrap();
    }

    #[test]
    fn list_dir_contract_holds_on_mem_provider() {
        let fs = MemFs::new();
        let root = Path::new("/proj");
        seed_layout(&fs, root);

        assert_list_dir_contract(&fs, root);
    }

    #[test]
    fn list_dir_contract_holds_on_local_provider() {
        let root = std::env::temp_dir().join(format!(
            "smelt-fs-seam-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let fs = LocalFs;
        fs.create_dir_all(&root).unwrap();
        seed_layout(&fs, &root);

        assert_list_dir_contract(&fs, &root);

        fs.remove_dir_all(&root).unwrap();
        assert!(!fs.exists(&root));
    }

    #[test]
    fn unreadable_dir_lists_as_empty_rather_than_erroring() {
        let fs = MemFs::new();
        assert_eq!(list_dir(&fs, Path::new("/missing")), Vec::new());
    }

    #[test]
    fn mem_provider_round_trips_read_write_remove() {
        let fs = MemFs::with_files([("/w/a.txt", "hello")]);

        assert_eq!(fs.read_to_string(Path::new("/w/a.txt")).unwrap(), "hello");
        assert!(fs.is_dir(Path::new("/w")));
        assert!(!fs.is_dir(Path::new("/w/a.txt")));

        fs.write(Path::new("/w/a.txt"), "bye").unwrap();
        assert_eq!(fs.read_to_string(Path::new("/w/a.txt")).unwrap(), "bye");

        fs.remove_file(Path::new("/w/a.txt")).unwrap();
        assert!(!fs.exists(Path::new("/w/a.txt")));
        assert_eq!(
            fs.read_to_string(Path::new("/w/a.txt")).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    /// 递归删除必须连子树一起清掉，否则 MemFs 会留下「父没了子还在」的
    /// 幽灵路径，测试就不再是真盘的可信替身。
    fn assert_remove_dir_all_clears_subtree(fs: &dyn FileSystem, root: &Path) {
        fs.create_dir_all(&root.join("nested/deep")).unwrap();
        fs.write(&root.join("nested/deep/f.txt"), "x").unwrap();

        fs.remove_dir_all(&root.join("nested")).unwrap();

        assert!(!fs.exists(&root.join("nested")));
        assert!(!fs.exists(&root.join("nested/deep")));
        assert!(!fs.exists(&root.join("nested/deep/f.txt")));
    }

    #[test]
    fn remove_dir_all_clears_subtree_on_both_providers() {
        let mem = MemFs::new();
        mem.create_dir_all(Path::new("/proj")).unwrap();
        assert_remove_dir_all_clears_subtree(&mem, Path::new("/proj"));

        let root = std::env::temp_dir().join(format!("smelt-fs-rm-{}", std::process::id()));
        let local = LocalFs;
        let _ = local.remove_dir_all(&root);
        local.create_dir_all(&root).unwrap();
        assert_remove_dir_all_clears_subtree(&local, &root);
        local.remove_dir_all(&root).unwrap();
    }

    // ===================== 新方法的双 Provider 契约测试 =====================

    /// create_dir 独占创建语义：父目录必须存在，目标已存在返回 AlreadyExists。
    fn assert_create_dir_contract(fs: &dyn FileSystem, root: &Path) {
        // 父目录存在时正常创建
        let dir = root.join("new_dir");
        assert!(fs.create_dir(&dir).is_ok());
        assert!(fs.is_dir(&dir));

        // 目标已存在返回 AlreadyExists
        let err = fs.create_dir(&dir).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);

        // 父目录不存在时失败
        let deep = root.join("missing/nested");
        let err = fs.create_dir(&deep).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn create_dir_contract_holds_on_both_providers() {
        let mem = MemFs::new();
        mem.create_dir_all(Path::new("/proj")).unwrap();
        assert_create_dir_contract(&mem, Path::new("/proj"));

        let root = std::env::temp_dir().join(format!("smelt-fs-mkdir-{}", std::process::id()));
        let local = LocalFs;
        let _ = local.remove_dir_all(&root);
        local.create_dir_all(&root).unwrap();
        assert_create_dir_contract(&local, &root);
        local.remove_dir_all(&root).unwrap();
    }

    /// rename 目录时必须搬移整个子树。
    fn assert_rename_dir_moves_subtree(fs: &dyn FileSystem, root: &Path) {
        // 创建目录结构
        fs.create_dir_all(&root.join("old/nested")).unwrap();
        fs.write(&root.join("old/a.txt"), "a").unwrap();
        fs.write(&root.join("old/nested/b.txt"), "b").unwrap();

        // 重命名目录
        fs.rename(&root.join("old"), &root.join("new")).unwrap();

        // 旧路径全部消失
        assert!(!fs.exists(&root.join("old")));
        assert!(!fs.exists(&root.join("old/a.txt")));
        assert!(!fs.exists(&root.join("old/nested")));
        assert!(!fs.exists(&root.join("old/nested/b.txt")));

        // 新路径全部出现
        assert!(fs.is_dir(&root.join("new")));
        assert_eq!(fs.read_to_string(&root.join("new/a.txt")).unwrap(), "a");
        assert!(fs.is_dir(&root.join("new/nested")));
        assert_eq!(
            fs.read_to_string(&root.join("new/nested/b.txt")).unwrap(),
            "b"
        );
    }

    #[test]
    fn rename_dir_moves_subtree_on_both_providers() {
        let mem = MemFs::new();
        mem.create_dir_all(Path::new("/proj")).unwrap();
        assert_rename_dir_moves_subtree(&mem, Path::new("/proj"));

        let root = std::env::temp_dir().join(format!("smelt-fs-rename-{}", std::process::id()));
        let local = LocalFs;
        let _ = local.remove_dir_all(&root);
        local.create_dir_all(&root).unwrap();
        assert_rename_dir_moves_subtree(&local, &root);
        local.remove_dir_all(&root).unwrap();
    }

    /// copy 复制文件内容。
    fn assert_copy_file_contract(fs: &dyn FileSystem, root: &Path) {
        fs.write(&root.join("src.txt"), "content").unwrap();
        fs.copy(&root.join("src.txt"), &root.join("dst.txt"))
            .unwrap();

        assert_eq!(fs.read_to_string(&root.join("src.txt")).unwrap(), "content");
        assert_eq!(fs.read_to_string(&root.join("dst.txt")).unwrap(), "content");

        // 修改源不影响目标
        fs.write(&root.join("src.txt"), "modified").unwrap();
        assert_eq!(fs.read_to_string(&root.join("dst.txt")).unwrap(), "content");
    }

    #[test]
    fn copy_file_contract_holds_on_both_providers() {
        let mem = MemFs::new();
        mem.create_dir_all(Path::new("/proj")).unwrap();
        assert_copy_file_contract(&mem, Path::new("/proj"));

        let root = std::env::temp_dir().join(format!("smelt-fs-copy-{}", std::process::id()));
        let local = LocalFs;
        let _ = local.remove_dir_all(&root);
        local.create_dir_all(&root).unwrap();
        assert_copy_file_contract(&local, &root);
        local.remove_dir_all(&root).unwrap();
    }
}
