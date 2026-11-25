//! 目录占用统计（`fs.usage.*`）：并行扫一遍目录树，把每个目录的汇总大小留在内存里，
//! 调用方按层取。大小口径见 `smelt_runtime_client::protocol::fs::FsUsageEntry`。

use std::collections::HashSet;
use std::fs::Metadata;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rayon::prelude::*;
use smelt_runtime_client::protocol::fs::{FsIdentity, FsKind, FsUsageEntry, FsUsageProgress};

use crate::fs_ops::{FsError, allocated_of, identity_of, kind_of, modified_of};

/// 进度最多隔这么久报一次。
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
/// 扫描线程数。瓶颈在磁盘和内核，线程再多也快不了，还会挤占别的活。
const SCAN_THREADS: usize = 8;

/// 一次扫描的开关和计数，扫描线程和调用方共用。
#[derive(Default)]
pub struct ScanControl {
    cancelled: AtomicBool,
    files: AtomicU64,
    bytes: AtomicU64,
}

impl ScanControl {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    pub fn progress(&self) -> FsUsageProgress {
        FsUsageProgress {
            files: self.files.load(Ordering::Relaxed),
            bytes: self.bytes.load(Ordering::Relaxed),
        }
    }
}

/// 树里只存目录：文件数量是目录的十来倍，全存下来一棵家目录的树要几百 MB。文件在取那一层
/// 时当场列（见 [`UsageTree::children`]）。
struct Node {
    name: Box<str>,
    kind: FsKind,
    size: u64,
    files: u64,
    unreadable: u64,
    modified: Option<i64>,
    identity: Option<FsIdentity>,
    /// 子目录。
    dirs: Box<[Node]>,
}

impl Node {
    fn new(name: &str, meta: &Metadata) -> Self {
        Self {
            name: name.into(),
            kind: kind_of(meta),
            size: allocated_of(meta),
            files: u64::from(meta.is_file()),
            unreadable: 0,
            modified: modified_of(meta),
            identity: identity_of(meta),
            dirs: Box::new([]),
        }
    }

    fn entry(&self) -> FsUsageEntry {
        FsUsageEntry {
            name: self.name.to_string(),
            kind: self.kind,
            size: self.size,
            files: self.files,
            unreadable: self.unreadable,
            modified: self.modified,
            identity: self.identity,
        }
    }
}

/// 扫完的一棵树。
pub struct UsageTree {
    root_path: PathBuf,
    root: Node,
}

impl UsageTree {
    /// `path` 那一项和它的直接子项，子项按大小从大到小。`path` 不在这棵树里时是 `None`。
    ///
    /// 子目录的大小是扫描时的；文件当场列，大小是现在的。扫完之后删掉的东西不再列出，但上面
    /// 各层的总数还是扫描时的。
    pub fn children(&self, path: &str) -> Option<(FsUsageEntry, Vec<FsUsageEntry>)> {
        let dir = Path::new(path);
        let rest = dir.strip_prefix(&self.root_path).ok()?;
        let mut node = &self.root;
        for component in rest.components() {
            let Component::Normal(name) = component else {
                return None;
            };
            let name = name.to_str()?;
            node = node.dirs.iter().find(|child| &*child.name == name)?;
        }
        let mut children: Vec<FsUsageEntry> = Vec::with_capacity(node.dirs.len());
        match std::fs::read_dir(dir) {
            Ok(entries) => {
                let mut live_dirs = HashSet::new();
                for entry in entries.flatten() {
                    let Ok(meta) = entry.metadata() else {
                        continue;
                    };
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if meta.is_dir() {
                        live_dirs.insert(name);
                    } else {
                        children.push(Node::new(&name, &meta).entry());
                    }
                }
                children.extend(
                    node.dirs
                        .iter()
                        .filter(|child| live_dirs.contains(&*child.name))
                        .map(Node::entry),
                );
            }
            // 现在读不了（被删了、权限变了）就只给扫描时的子目录。
            Err(_) => children.extend(node.dirs.iter().map(Node::entry)),
        }
        children.sort_by(|a, b| b.size.cmp(&a.size));
        Some((node.entry(), children))
    }
}

struct Walk<'a> {
    control: &'a ScanControl,
    /// 能进的设备。不进别的卷，macOS 上系统卷和数据卷算一个（见 [`allowed_devices`]）。
    devices: Vec<u64>,
    /// 已经算过的目录和多链接文件，经别的路径再碰到就不重复算。
    seen: Mutex<HashSet<(u64, u64)>>,
    last_report: Mutex<Instant>,
    on_progress: &'a (dyn Fn(FsUsageProgress) + Sync),
}

impl Walk<'_> {
    fn first_visit(&self, meta: &Metadata) -> bool {
        match identity_of(meta) {
            Some(id) => self
                .seen
                .lock()
                .expect("seen set lock poisoned")
                .insert((id.dev, id.ino)),
            None => true,
        }
    }

    fn same_volume(&self, meta: &Metadata) -> bool {
        match identity_of(meta) {
            Some(id) => self.devices.is_empty() || self.devices.contains(&id.dev),
            None => true,
        }
    }

    fn counted(&self, files: u64, bytes: u64) {
        self.control.files.fetch_add(files, Ordering::Relaxed);
        self.control.bytes.fetch_add(bytes, Ordering::Relaxed);
        // 别的线程正在报就不等它。
        let Ok(mut last) = self.last_report.try_lock() else {
            return;
        };
        if last.elapsed() >= PROGRESS_INTERVAL {
            *last = Instant::now();
            (self.on_progress)(self.control.progress());
        }
    }

    fn dir(&self, path: &Path, name: &str, meta: &Metadata) -> Node {
        let mut node = Node::new(name, meta);
        if self.control.is_cancelled() {
            return node;
        }
        let entries = match std::fs::read_dir(path) {
            Ok(entries) => entries,
            Err(_) => {
                node.unreadable = 1;
                return node;
            }
        };
        let mut subdirs = Vec::new();
        let (mut files, mut bytes) = (0, 0);
        for entry in entries {
            let Ok(entry) = entry else {
                node.unreadable += 1;
                continue;
            };
            // 不跟随符号链接。列的过程中被删掉的拿不到元数据，跳过。
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                subdirs.push((entry.path(), entry.file_name(), meta));
                continue;
            }
            let size = if is_multiply_linked(&meta) && !self.first_visit(&meta) {
                0
            } else {
                allocated_of(&meta)
            };
            files += u64::from(meta.is_file());
            bytes += size;
        }
        node.size += bytes;
        node.files = files;
        self.counted(files, bytes);

        let mut dirs: Vec<Node> = subdirs
            .into_par_iter()
            .map(|(path, name, meta)| {
                let name = name.to_string_lossy();
                if self.same_volume(&meta) && self.first_visit(&meta) {
                    self.dir(&path, &name, &meta)
                } else {
                    // 别的卷的挂载点，或者经别的路径已经算过的目录：列出来，不算大小。
                    let mut skipped = Node::new(&name, &meta);
                    skipped.size = 0;
                    skipped
                }
            })
            .collect();
        for child in &dirs {
            node.size += child.size;
            node.files += child.files;
            node.unreadable += child.unreadable;
        }
        dirs.sort_by(|a, b| b.size.cmp(&a.size));
        node.dirs = dirs.into_boxed_slice();
        node
    }
}

#[cfg(unix)]
fn is_multiply_linked(meta: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    meta.nlink() > 1
}

#[cfg(not(unix))]
fn is_multiply_linked(_meta: &Metadata) -> bool {
    false
}

/// 扫描能进的设备。macOS 的根目录在只读的系统卷上，`/Users`、`/Applications` 这些经
/// firmlink 落在数据卷上，两个卷同属一个容器，用户眼里是一块盘。
fn allowed_devices(root: &Metadata) -> Vec<u64> {
    let Some(root) = identity_of(root) else {
        return Vec::new();
    };
    let mut devices = vec![root.dev];
    if cfg!(target_os = "macos") {
        let pair: Vec<u64> = ["/", "/System/Volumes/Data"]
            .into_iter()
            .filter_map(|path| std::fs::metadata(path).ok())
            .filter_map(|meta| identity_of(&meta).map(|id| id.dev))
            .collect();
        if pair.contains(&root.dev) {
            devices.extend(pair);
        }
    }
    devices
}

/// 扫 `root`（跟随它本身的符号链接，下面的不跟随）。被取消时返回 `None`。
pub fn scan(
    root: &str,
    control: &ScanControl,
    on_progress: &(dyn Fn(FsUsageProgress) + Sync),
) -> Result<Option<UsageTree>, FsError> {
    let root_path = PathBuf::from(root);
    if !root_path.is_absolute() || root_path.components().any(|c| c == Component::ParentDir) {
        return Err(FsError::InvalidPath(root.to_string()));
    }
    let meta = std::fs::metadata(&root_path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => FsError::NotFound(root.to_string()),
        std::io::ErrorKind::PermissionDenied => FsError::PermissionDenied(root.to_string()),
        _ => FsError::Io(format!("{root}: {error}")),
    })?;
    let name = root_path
        .file_name()
        .map_or_else(|| root.to_string(), |n| n.to_string_lossy().into_owned());
    let walk = Walk {
        control,
        devices: allowed_devices(&meta),
        seen: Mutex::new(HashSet::new()),
        last_report: Mutex::new(Instant::now()),
        on_progress,
    };
    let node = if meta.is_dir() {
        walk.first_visit(&meta);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(SCAN_THREADS)
            .thread_name(|i| format!("smelt-disk-usage-{i}"))
            .build()
            .map_err(|error| FsError::Io(error.to_string()))?;
        pool.install(|| walk.dir(&root_path, &name, &meta))
    } else {
        let leaf = Node::new(&name, &meta);
        walk.counted(leaf.files, leaf.size);
        leaf
    };
    if control.is_cancelled() {
        return Ok(None);
    }
    Ok(Some(UsageTree {
        root_path,
        root: node,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "smelt-disk-usage-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn scan_all(root: &Path) -> UsageTree {
        scan(root.to_str().unwrap(), &ScanControl::default(), &|_| {})
            .unwrap()
            .unwrap()
    }

    fn names(children: &[FsUsageEntry]) -> Vec<&str> {
        children.iter().map(|c| c.name.as_str()).collect()
    }

    #[test]
    fn sizes_roll_up_and_children_are_largest_first() {
        let root = temp_dir("rollup");
        std::fs::create_dir_all(root.join("big/inner")).unwrap();
        std::fs::write(root.join("big/inner/a.bin"), vec![1u8; 256 * 1024]).unwrap();
        std::fs::write(root.join("big/b.bin"), vec![1u8; 64 * 1024]).unwrap();
        std::fs::write(root.join("small.txt"), "x").unwrap();

        let control = ScanControl::default();
        let tree = scan(root.to_str().unwrap(), &control, &|_| {})
            .unwrap()
            .unwrap();
        let (entry, children) = tree.children(root.to_str().unwrap()).unwrap();
        assert_eq!(entry.files, 3);
        assert_eq!(names(&children), vec!["big", "small.txt"]);
        assert_eq!(
            entry.size,
            children.iter().map(|c| c.size).sum::<u64>() + {
                // 根目录自己占的块。
                allocated_of(&std::fs::metadata(&root).unwrap())
            }
        );
        assert!(children[0].size >= 320 * 1024);
        assert_eq!(control.progress().files, 3);

        let (inner, grandchildren) = tree.children(root.join("big").to_str().unwrap()).unwrap();
        assert_eq!(inner.name, "big");
        assert_eq!(names(&grandchildren), vec!["inner", "b.bin"]);
        assert!(tree.children("/elsewhere").is_none());
        assert!(tree.children(root.join("nope").to_str().unwrap()).is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn hard_links_and_symlinks_are_not_counted_twice() {
        let root = temp_dir("links");
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::write(root.join("a/data.bin"), vec![1u8; 128 * 1024]).unwrap();
        std::fs::hard_link(root.join("a/data.bin"), root.join("hard.bin")).unwrap();
        std::os::unix::fs::symlink(root.join("a"), root.join("link")).unwrap();

        let tree = scan_all(&root);
        let (_, children) = tree.children(root.to_str().unwrap()).unwrap();
        let data: u64 = children
            .iter()
            .filter(|c| c.name == "a" || c.name == "hard.bin")
            .map(|c| c.size)
            .sum();
        let one_copy = allocated_of(&std::fs::metadata(root.join("a/data.bin")).unwrap());
        let a_dir = allocated_of(&std::fs::metadata(root.join("a")).unwrap());
        assert_eq!(data, one_copy + a_dir, "硬链接只算一份");
        let link = children.iter().find(|c| c.name == "link").unwrap();
        assert_eq!(link.kind, FsKind::Symlink);
        assert!(link.size < one_copy, "符号链接不跟进去");
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_dirs_are_reported_not_treated_as_empty() {
        use std::os::unix::fs::PermissionsExt as _;
        // root 跑测试时权限不起作用。
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let root = temp_dir("denied");
        std::fs::create_dir_all(root.join("locked")).unwrap();
        std::fs::write(root.join("locked/secret"), "x").unwrap();
        std::fs::set_permissions(root.join("locked"), std::fs::Permissions::from_mode(0o000))
            .unwrap();

        let tree = scan_all(&root);
        let (entry, children) = tree.children(root.to_str().unwrap()).unwrap();
        assert_eq!(entry.unreadable, 1);
        assert_eq!(children[0].name, "locked");
        assert_eq!(children[0].unreadable, 1);

        std::fs::set_permissions(root.join("locked"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cancelled_scan_returns_nothing() {
        let root = temp_dir("cancel");
        std::fs::create_dir_all(root.join("a")).unwrap();
        let control = ScanControl::default();
        control.cancel();
        assert!(
            scan(root.to_str().unwrap(), &control, &|_| {})
                .unwrap()
                .is_none()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn missing_root_is_an_error() {
        // 要绝对路径：Windows 上 `/definitely/not/here` 不算绝对，先报 InvalidPath。
        let missing = std::env::temp_dir().join(format!("smelt-missing-{}", uuid::Uuid::new_v4()));
        assert!(matches!(
            scan(&missing.to_string_lossy(), &ScanControl::default(), &|_| {}),
            Err(FsError::NotFound(_))
        ));
        assert!(matches!(
            scan("relative", &ScanControl::default(), &|_| {}),
            Err(FsError::InvalidPath(_))
        ));
    }
}

#[cfg(test)]
mod manual {
    /// 手动量一下真实目录：`SMELT_SCAN_ROOT=~ cargo test -p smelt-core --release manual_scan -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn manual_scan() {
        let root = std::env::var("SMELT_SCAN_ROOT").unwrap();
        let started = std::time::Instant::now();
        let control = super::ScanControl::default();
        let tree = super::scan(&root, &control, &|p| eprintln!("progress {p:?}"))
            .unwrap()
            .unwrap();
        let (entry, children) = tree.children(&root).unwrap();
        eprintln!("{:?} in {:?}", entry, started.elapsed());
        for child in children.iter().take(10) {
            eprintln!(
                "  {:>12} {} unreadable={}",
                child.size, child.name, child.unreadable
            );
        }
    }
}
