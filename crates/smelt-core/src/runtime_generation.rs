//! Content-addressed, immutable managed-runtime generations.
//!
//! Published generation directories are never modified. Their lease inode lives outside the
//! content tree so GC can remove a generation while retaining one stable synchronization object.

#![cfg(unix)]

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    fs::{self, File, OpenOptions},
    io::Write as _,
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::{fs::OpenOptionsExt as _, process::CommandExt as _},
    },
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

pub const GENERATION_SCHEMA: u32 = 1;
const READY_FILE: &str = "READY";
const MANIFEST_FILE: &str = "manifest.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenerationKind {
    Bun,
    Pi,
}

impl GenerationKind {
    fn directory(self) -> &'static str {
        match self {
            Self::Bun => "bun",
            Self::Pi => "pi",
        }
    }
}

#[derive(Clone, Debug)]
pub struct GenerationStore {
    runtime: PathBuf,
}

impl GenerationStore {
    pub fn managed() -> Result<Self, String> {
        let home = dirs::home_dir().ok_or("找不到 home 目录")?;
        Ok(Self::new(home.join(".smelt/runtime")))
    }

    pub fn new(runtime: PathBuf) -> Self {
        Self { runtime }
    }

    pub fn runtime_root(&self) -> &Path {
        &self.runtime
    }

    pub fn generation_root(&self, kind: GenerationKind, id: &str) -> PathBuf {
        self.runtime.join("store").join(kind.directory()).join(id)
    }

    fn store_root(&self, kind: GenerationKind) -> PathBuf {
        self.runtime.join("store").join(kind.directory())
    }

    fn lease_path(&self, kind: GenerationKind, id: &str) -> PathBuf {
        self.runtime
            .join("leases")
            .join(kind.directory())
            .join(format!("{id}.lock"))
    }

    fn staging_root(&self) -> PathBuf {
        self.runtime.join("staging")
    }

    pub fn lock_manager(&self) -> Result<RuntimeManagerLock, String> {
        self.open_manager_lock(false)
    }

    pub fn try_lock_manager(&self) -> Result<Option<RuntimeManagerLock>, String> {
        fs::create_dir_all(&self.runtime)
            .map_err(|error| format!("创建受管运行时目录失败：{error}"))?;
        let path = self.runtime.join(".generation.lock");
        let file =
            open_lock_file(&path).map_err(|error| format!("打开受管运行时管理锁失败：{error}"))?;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(Some(RuntimeManagerLock { _file: file }));
        }
        let error = std::io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(libc::EWOULDBLOCK)) {
            Ok(None)
        } else {
            Err(format!("锁定受管运行时目录失败：{error}"))
        }
    }

    fn open_manager_lock(&self, nonblocking: bool) -> Result<RuntimeManagerLock, String> {
        fs::create_dir_all(&self.runtime)
            .map_err(|error| format!("创建受管运行时目录失败：{error}"))?;
        let path = self.runtime.join(".generation.lock");
        let file =
            open_lock_file(&path).map_err(|error| format!("打开受管运行时管理锁失败：{error}"))?;
        debug_assert!(
            !nonblocking,
            "nonblocking acquisition uses try_lock_manager"
        );
        flock(&file, libc::LOCK_EX, "锁定受管运行时目录")?;
        Ok(RuntimeManagerLock { _file: file })
    }

    /// Must be called while the manager lock is held. The caller keeps that lock until this
    /// returns, closing the check/open-vs-GC race.
    pub fn acquire_shared_locked(
        &self,
        kind: GenerationKind,
        id: &str,
    ) -> Result<GenerationLease, String> {
        validate_id(id)?;
        let root = self.generation_root(kind, id);
        if !published_marker_is_valid(&root) {
            return Err(format!(
                "受管运行时 generation 未完整发布：{}",
                root.display()
            ));
        }
        let path = self.lease_path(kind, id);
        let parent = path.parent().ok_or("generation lease 没有父目录")?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("创建 generation lease 目录失败：{error}"))?;
        let file = open_lock_file(&path)
            .map_err(|error| format!("打开 generation lease 失败：{error}"))?;
        flock(&file, libc::LOCK_SH, "钉住受管运行时 generation")?;
        let fd: OwnedFd = file.into();
        Ok(GenerationLease {
            kind,
            id: id.to_string(),
            root,
            fd: Arc::new(fd),
        })
    }

    pub fn acquire_shared(
        &self,
        kind: GenerationKind,
        id: &str,
    ) -> Result<GenerationLease, String> {
        let _manager = self.lock_manager()?;
        self.acquire_shared_locked(kind, id)
    }

    pub fn read_manifest_locked<T: DeserializeOwned>(
        &self,
        kind: GenerationKind,
        id: &str,
    ) -> Result<T, String> {
        let root = self.generation_root(kind, id);
        if !published_marker_is_valid(&root) {
            return Err(format!("generation 未完整发布：{}", root.display()));
        }
        let bytes = fs::read(root.join(MANIFEST_FILE))
            .map_err(|error| format!("读取 generation manifest 失败：{error}"))?;
        serde_json::from_slice(&bytes)
            .map_err(|error| format!("解析 generation manifest 失败：{error}"))
    }

    pub fn write_current_locked(&self, kind: GenerationKind, id: &str) -> Result<(), String> {
        validate_id(id)?;
        if !published_marker_is_valid(&self.generation_root(kind, id)) {
            return Err("不能把 current 指向未完整发布的 generation".to_string());
        }
        let current_root = self.runtime.join("current");
        fs::create_dir_all(&current_root)
            .map_err(|error| format!("创建 generation current 目录失败：{error}"))?;
        let target = current_root.join(kind.directory());
        let temporary = current_root.join(format!(
            ".{}-{}-{}",
            kind.directory(),
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let result = (|| {
            write_new_file(&temporary, format!("{id}\n").as_bytes())?;
            fs::rename(&temporary, &target)
                .map_err(|error| format!("原子切换 generation current 失败：{error}"))?;
            sync_directory(&current_root)
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }

    pub fn current_locked(&self, kind: GenerationKind) -> Option<String> {
        let value = fs::read_to_string(self.runtime.join("current").join(kind.directory())).ok()?;
        let id = value.trim();
        validate_id(id).ok()?;
        published_marker_is_valid(&self.generation_root(kind, id)).then(|| id.to_string())
    }

    /// Move a damaged tree out of the public namespace only after proving that no process holds
    /// its lease. The replacement is published afresh; the old tree is never edited in place.
    pub fn discard_invalid_locked(&self, kind: GenerationKind, id: &str) -> Result<(), String> {
        validate_id(id)?;
        let root = self.generation_root(kind, id);
        if fs::symlink_metadata(&root).is_err() {
            return Ok(());
        }
        let exclusive = self
            .try_exclusive_locked(kind, id)
            .map_err(|error| format!("generation 已损坏但仍被进程占用，拒绝替换：{error}"))?;
        fs::create_dir_all(self.staging_root())
            .map_err(|error| format!("创建 generation 隔离目录失败：{error}"))?;
        let quarantined = self.staging_root().join(format!(
            ".quarantine-{}-{}-{}",
            kind.directory(),
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::rename(&root, &quarantined)
            .map_err(|error| format!("隔离损坏 generation 失败：{error}"))?;
        sync_directory(&self.store_root(kind))?;
        drop(exclusive);
        fs::remove_dir_all(&quarantined)
            .map_err(|error| format!("删除已隔离 generation 失败：{error}"))
    }

    /// Build a private tree, make every byte durable, then expose it with one directory rename.
    /// The caller must hold the manager lock for this entire operation.
    pub fn publish_locked<T: Serialize>(
        &self,
        kind: GenerationKind,
        id: &str,
        manifest: &T,
        build: impl FnOnce(&Path) -> Result<(), String>,
    ) -> Result<PathBuf, String> {
        validate_id(id)?;
        let final_root = self.generation_root(kind, id);
        if published_marker_is_valid(&final_root) {
            return Ok(final_root);
        }
        if fs::symlink_metadata(&final_root).is_ok() {
            return Err(format!(
                "拒绝覆盖不完整或异常 generation：{}",
                final_root.display()
            ));
        }
        let store_root = self.store_root(kind);
        let staging_root = self.staging_root();
        fs::create_dir_all(&store_root)
            .map_err(|error| format!("创建 generation store 失败：{error}"))?;
        fs::create_dir_all(&staging_root)
            .map_err(|error| format!("创建 generation staging 失败：{error}"))?;
        let staging = staging_root.join(format!(
            ".{}-{}-{}",
            kind.directory(),
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&staging)
            .map_err(|error| format!("创建私有 generation staging 失败：{error}"))?;
        let result = (|| {
            build(&staging)?;
            let manifest_bytes = serde_json::to_vec_pretty(manifest)
                .map_err(|error| format!("编码 generation manifest 失败：{error}"))?;
            write_new_file(&staging.join(MANIFEST_FILE), &manifest_bytes)?;
            write_new_file(&staging.join(READY_FILE), b"ready\n")?;
            sync_tree(&staging)?;
            fs::rename(&staging, &final_root)
                .map_err(|error| format!("原子发布 generation 失败：{error}"))?;
            sync_directory(&store_root)?;
            Ok(final_root.clone())
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&staging);
        }
        result
    }

    /// Remove only content trees whose external lease can be taken exclusively. Lease files are
    /// deliberately permanent: unlinking them would let old and new processes lock two inodes.
    pub fn gc_kind_locked(&self, kind: GenerationKind, keep: &std::collections::HashSet<String>) {
        let root = self.store_root(kind);
        let Ok(entries) = fs::read_dir(&root) else {
            return;
        };
        for entry in entries.flatten() {
            let Some(id) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if keep.contains(&id) || validate_id(&id).is_err() {
                continue;
            }
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Ok(exclusive) = self.try_exclusive_locked(kind, &id) else {
                continue;
            };
            let _ = fs::remove_dir_all(&path);
            drop(exclusive);
        }
        let _ = sync_directory(&root);
    }

    fn try_exclusive_locked(
        &self,
        kind: GenerationKind,
        id: &str,
    ) -> Result<ExclusiveGenerationLease, String> {
        let path = self.lease_path(kind, id);
        let parent = path.parent().ok_or("generation lease 没有父目录")?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("创建 generation lease 目录失败：{error}"))?;
        let file = open_lock_file(&path)
            .map_err(|error| format!("打开 generation GC lease 失败：{error}"))?;
        flock(&file, libc::LOCK_EX | libc::LOCK_NB, "generation 仍在使用")?;
        Ok(ExclusiveGenerationLease { _file: file })
    }

    pub fn remove_stale_staging_locked(&self) {
        let root = self.staging_root();
        let Ok(entries) = fs::read_dir(root) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let _ = fs::remove_dir_all(path);
            } else {
                let _ = fs::remove_file(path);
            }
        }
    }
}

pub struct RuntimeManagerLock {
    _file: File,
}

impl RuntimeManagerLock {
    /// Keep the manager transaction alive if the supervisor dies while a staging subprocess is
    /// still writing. This is only for bounded build/download children, never runtime sessions.
    pub fn inherit_into(&self, command: &mut Command) {
        inherit_raw_fds(command, &[self._file.as_raw_fd()]);
    }
}

#[derive(Clone, Debug)]
pub struct GenerationLease {
    kind: GenerationKind,
    id: String,
    root: PathBuf,
    fd: Arc<OwnedFd>,
}

impl GenerationLease {
    pub fn kind(&self) -> GenerationKind {
        self.kind
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn inherited_fd(&self) -> Arc<OwnedFd> {
        Arc::clone(&self.fd)
    }
}

struct ExclusiveGenerationLease {
    _file: File,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BunGenerationManifest {
    pub schema: u32,
    pub generation_id: String,
    pub version: String,
    pub platform: String,
    pub arch: String,
    pub executable_sha256: String,
    pub download_url: String,
    pub archive_sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PiGenerationManifest {
    pub schema: u32,
    pub generation_id: String,
    pub runtime_version: String,
    pub platform: String,
    pub arch: String,
    pub bridge_protocol: String,
    pub source_sha256: String,
    pub dependency_sha256: String,
    pub bun_generation_id: String,
}

pub fn inherit_generation_fds(command: &mut Command, leases: &[GenerationLease]) {
    inherit_raw_fds(
        command,
        &leases
            .iter()
            .map(|lease| lease.fd.as_raw_fd())
            .collect::<Vec<_>>(),
    );
}

fn inherit_raw_fds(command: &mut Command, fds: &[std::os::fd::RawFd]) {
    let fds = fds.to_vec();
    // SAFETY: the closure runs after fork in the child. It only calls async-signal-safe fcntl and
    // never changes the parent's CLOEXEC flags, so concurrent unrelated spawns cannot leak locks.
    unsafe {
        command.pre_exec(move || {
            for fd in &fds {
                let flags = libc::fcntl(*fd, libc::F_GETFD);
                if flags < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::fcntl(*fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
}

fn open_lock_file(path: &Path) -> std::io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("lock path is not a regular file"));
    }
    Ok(file)
}

fn flock(file: &File, operation: libc::c_int, action: &str) -> Result<(), String> {
    if unsafe { libc::flock(file.as_raw_fd(), operation) } == 0 {
        Ok(())
    } else {
        Err(format!("{action}失败：{}", std::io::Error::last_os_error()))
    }
}

fn validate_id(id: &str) -> Result<(), String> {
    let valid = !id.is_empty()
        && id.len() <= 96
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    valid
        .then_some(())
        .ok_or_else(|| format!("非法 generation id：{id}"))
}

fn published_marker_is_valid(root: &Path) -> bool {
    let Ok(root_meta) = fs::symlink_metadata(root) else {
        return false;
    };
    if root_meta.file_type().is_symlink() || !root_meta.is_dir() {
        return false;
    }
    [MANIFEST_FILE, READY_FILE].into_iter().all(|name| {
        fs::symlink_metadata(root.join(name))
            .is_ok_and(|meta| meta.is_file() && !meta.file_type().is_symlink())
    })
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| format!("创建 {} 失败：{error}", path.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("写入 {} 失败：{error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("持久化 {} 失败：{error}", path.display()))
}

fn sync_tree(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("读取 generation 元数据失败：{error}"))?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_file() {
        File::open(path)
            .and_then(|file| file.sync_all())
            .map_err(|error| format!("持久化 generation 文件 {} 失败：{error}", path.display()))?;
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path)
            .map_err(|error| format!("枚举 generation 目录 {} 失败：{error}", path.display()))?
        {
            let entry = entry.map_err(|error| {
                format!("枚举 generation 目录 {} 失败：{error}", path.display())
            })?;
            sync_tree(&entry.path())?;
        }
        sync_directory(path)?;
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), String> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("持久化目录 {} 失败：{error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::HashSet, process::Stdio, time::Duration};

    fn manifest(id: &str) -> BunGenerationManifest {
        BunGenerationManifest {
            schema: GENERATION_SCHEMA,
            generation_id: id.to_string(),
            version: "test".into(),
            platform: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            executable_sha256: "00".repeat(32),
            download_url: "https://example.invalid/bun.zip".into(),
            archive_sha256: "11".repeat(32),
        }
    }

    #[test]
    fn publication_exposes_only_a_complete_immutable_tree() {
        let temp = tempfile::tempdir().unwrap();
        let store = GenerationStore::new(temp.path().join("runtime"));
        let _manager = store.lock_manager().unwrap();
        let id = "bun-aaaaaaaa";
        let final_root = store.generation_root(GenerationKind::Bun, id);
        store
            .publish_locked(GenerationKind::Bun, id, &manifest(id), |staging| {
                assert!(!final_root.exists(), "staging 不能占用最终可见路径");
                fs::write(staging.join("bun"), b"complete").unwrap();
                Ok(())
            })
            .unwrap();
        assert_eq!(fs::read(final_root.join("bun")).unwrap(), b"complete");
        assert!(final_root.join(MANIFEST_FILE).is_file());
        assert!(final_root.join(READY_FILE).is_file());
    }

    /// Re-executed by `child_process_acquired_shared_lease_blocks_exclusive_gc`.
    /// The subprocess opens the stable lease inode itself; this is intentionally distinct from
    /// inheriting the parent's already-acquired flock (which macOS treats differently).
    #[test]
    fn child_acquires_shared_lease() {
        let (Some(runtime_root), Some(id), Some(ready_path), Some(release_path)) = (
            std::env::var_os("SMELT_GENERATION_CHILD_RUNTIME"),
            std::env::var_os("SMELT_GENERATION_CHILD_ID"),
            std::env::var_os("SMELT_GENERATION_CHILD_READY"),
            std::env::var_os("SMELT_GENERATION_CHILD_RELEASE"),
        ) else {
            return;
        };
        let store = GenerationStore::new(PathBuf::from(runtime_root));
        let _lease = store
            .acquire_shared(GenerationKind::Bun, id.to_string_lossy().as_ref())
            .unwrap();
        fs::write(&ready_path, b"ready").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while !Path::new(&release_path).exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "parent did not release child"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn child_process_acquired_shared_lease_blocks_exclusive_gc() {
        let temp = tempfile::tempdir().unwrap();
        let store = GenerationStore::new(temp.path().join("runtime"));
        let id = "bun-bbbbbbbb";
        {
            let _manager = store.lock_manager().unwrap();
            store
                .publish_locked(GenerationKind::Bun, id, &manifest(id), |staging| {
                    fs::write(staging.join("bun"), b"runtime").map_err(|e| e.to_string())
                })
                .unwrap();
        }
        let ready = temp.path().join("child.ready");
        let release = temp.path().join("child.release");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "runtime_generation::tests::child_acquires_shared_lease",
                "--nocapture",
            ])
            .env("SMELT_GENERATION_CHILD_RUNTIME", store.runtime_root())
            .env("SMELT_GENERATION_CHILD_ID", id)
            .env("SMELT_GENERATION_CHILD_READY", &ready)
            .env("SMELT_GENERATION_CHILD_RELEASE", &release)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !ready.exists() {
            if let Some(status) = child.try_wait().unwrap() {
                panic!("lease child exited before acquiring lock: {status}");
            }
            assert!(
                std::time::Instant::now() < deadline,
                "child did not acquire lease"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        {
            let _manager = store.lock_manager().unwrap();
            store.gc_kind_locked(GenerationKind::Bun, &HashSet::new());
        }
        assert!(
            store.generation_root(GenerationKind::Bun, id).exists(),
            "子进程独立取得的 shared lease 必须阻止 exclusive GC"
        );

        fs::write(&release, b"release").unwrap();
        assert!(child.wait().unwrap().success());
        {
            let _manager = store.lock_manager().unwrap();
            store.gc_kind_locked(GenerationKind::Bun, &HashSet::new());
        }
        assert!(!store.generation_root(GenerationKind::Bun, id).exists());
        assert!(store.lease_path(GenerationKind::Bun, id).exists());
    }

    #[test]
    fn real_child_shared_lease_blocks_cross_process_gc() {
        if std::env::var_os("SMELT_GENERATION_LEASE_HELPER").is_some() {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let store = GenerationStore::new(temp.path().join("runtime"));
        let id = "bun-bbbbbbbb";
        {
            let _manager = store.lock_manager().unwrap();
            store
                .publish_locked(GenerationKind::Bun, id, &manifest(id), |staging| {
                    fs::write(staging.join("bun"), b"runtime").map_err(|e| e.to_string())
                })
                .unwrap();
        }
        let lease = store.acquire_shared(GenerationKind::Bun, id).unwrap();
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "printf ready; exec sleep 30"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env("SMELT_GENERATION_LEASE_HELPER", "1");
        inherit_generation_fds(&mut command, std::slice::from_ref(&lease));
        let mut child = command.spawn().unwrap();
        let mut ready = [0_u8; 5];
        use std::io::Read as _;
        child
            .stdout
            .as_mut()
            .unwrap()
            .read_exact(&mut ready)
            .unwrap();
        assert_eq!(&ready, b"ready");
        drop(lease);

        {
            let _manager = store.lock_manager().unwrap();
            store.gc_kind_locked(GenerationKind::Bun, &HashSet::new());
        }
        assert!(
            store.generation_root(GenerationKind::Bun, id).exists(),
            "exec 后的真实子进程必须继续持有 shared lease"
        );
        let _ = child.kill();
        let _ = child.wait();
        std::thread::sleep(Duration::from_millis(20));
        {
            let _manager = store.lock_manager().unwrap();
            store.gc_kind_locked(GenerationKind::Bun, &HashSet::new());
        }
        assert!(!store.generation_root(GenerationKind::Bun, id).exists());
        assert!(
            store.lease_path(GenerationKind::Bun, id).exists(),
            "外部 lease inode 必须永久保留"
        );
    }

    /// Helper process that exits immediately after handing its manager-lock FD to a builder.
    #[test]
    fn manager_lock_supervisor_exits_after_spawning_builder() {
        let (Some(runtime), Some(pid_file)) = (
            std::env::var_os("SMELT_GENERATION_MANAGER_WORKER_RUNTIME"),
            std::env::var_os("SMELT_GENERATION_MANAGER_WORKER_PID"),
        ) else {
            return;
        };
        let store = GenerationStore::new(PathBuf::from(runtime));
        let manager = store.lock_manager().unwrap();
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "exec sleep 30"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        manager.inherit_into(&mut command);
        let child = command.spawn().unwrap();
        fs::write(pid_file, child.id().to_string()).unwrap();
        drop(child); // Deliberately do not wait: this helper process is the simulated supervisor.
        drop(manager);
    }

    /// The child keeps the exclusive manager transaction alive after its supervisor exits.
    #[test]
    fn inherited_manager_lock_survives_supervisor_process_exit() {
        let temp = tempfile::tempdir().unwrap();
        let store = GenerationStore::new(temp.path().join("runtime"));
        let pid_file = temp.path().join("builder.pid");
        let helper = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "runtime_generation::tests::manager_lock_supervisor_exits_after_spawning_builder",
                "--nocapture",
            ])
            .env(
                "SMELT_GENERATION_MANAGER_WORKER_RUNTIME",
                store.runtime_root(),
            )
            .env("SMELT_GENERATION_MANAGER_WORKER_PID", &pid_file)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        assert!(helper.wait_with_output().unwrap().status.success());
        let pid: i32 = fs::read_to_string(&pid_file).unwrap().parse().unwrap();
        let mut orphan = OrphanProcess(pid);
        assert!(store.try_lock_manager().unwrap().is_none());
        orphan.kill();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(_manager) = store.try_lock_manager().unwrap() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "builder child did not release manager lock"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Helper process that exits after passing its shared generation lease to a child.
    #[test]
    fn lease_supervisor_exits_after_spawning_runtime_child() {
        let (Some(runtime), Some(id), Some(pid_file)) = (
            std::env::var_os("SMELT_GENERATION_LEASE_WORKER_RUNTIME"),
            std::env::var_os("SMELT_GENERATION_LEASE_WORKER_ID"),
            std::env::var_os("SMELT_GENERATION_LEASE_WORKER_PID"),
        ) else {
            return;
        };
        let store = GenerationStore::new(PathBuf::from(runtime));
        let lease = store
            .acquire_shared(GenerationKind::Bun, id.to_string_lossy().as_ref())
            .unwrap();
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "exec sleep 30"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        inherit_generation_fds(&mut command, std::slice::from_ref(&lease));
        let child = command.spawn().unwrap();
        fs::write(pid_file, child.id().to_string()).unwrap();
        drop(child); // Simulate a supervisor killed immediately after spawn.
        drop(lease);
    }

    #[test]
    fn inherited_lease_fd_survives_supervisor_process_exit_and_exec() {
        let temp = tempfile::tempdir().unwrap();
        let store = GenerationStore::new(temp.path().join("runtime"));
        let id = "bun-cccccccc";
        {
            let _manager = store.lock_manager().unwrap();
            store
                .publish_locked(GenerationKind::Bun, id, &manifest(id), |staging| {
                    fs::write(staging.join("bun"), b"runtime").map_err(|e| e.to_string())
                })
                .unwrap();
        }
        let pid_file = temp.path().join("runtime-child.pid");
        let helper = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "runtime_generation::tests::lease_supervisor_exits_after_spawning_runtime_child",
                "--nocapture",
            ])
            .env(
                "SMELT_GENERATION_LEASE_WORKER_RUNTIME",
                store.runtime_root(),
            )
            .env("SMELT_GENERATION_LEASE_WORKER_ID", id)
            .env("SMELT_GENERATION_LEASE_WORKER_PID", &pid_file)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        assert!(helper.wait_with_output().unwrap().status.success());
        let pid: i32 = fs::read_to_string(&pid_file).unwrap().parse().unwrap();
        let mut orphan = OrphanProcess(pid);

        {
            let _manager = store.lock_manager().unwrap();
            store.gc_kind_locked(GenerationKind::Bun, &HashSet::new());
        }
        assert!(
            store.generation_root(GenerationKind::Bun, id).exists(),
            "supervisor 已退出后，exec child 的 inherited shared lease 仍必须阻止 GC"
        );
        orphan.kill();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            {
                let _manager = store.lock_manager().unwrap();
                store.gc_kind_locked(GenerationKind::Bun, &HashSet::new());
            }
            if !store.generation_root(GenerationKind::Bun, id).exists() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "runtime child did not release lease"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(store.lease_path(GenerationKind::Bun, id).exists());
    }

    struct OrphanProcess(i32);

    impl OrphanProcess {
        fn kill(&mut self) {
            if self.0 > 1 {
                unsafe { libc::kill(self.0, libc::SIGKILL) };
                self.0 = 0;
            }
        }
    }

    impl Drop for OrphanProcess {
        fn drop(&mut self) {
            self.kill();
        }
    }

    /// Re-executed in two real processes to contend for the same generation identity.
    #[test]
    fn concurrent_publish_worker() {
        let (Some(runtime), Some(id), Some(gate), Some(started), Some(payload)) = (
            std::env::var_os("SMELT_GENERATION_WORKER_RUNTIME"),
            std::env::var_os("SMELT_GENERATION_WORKER_ID"),
            std::env::var_os("SMELT_GENERATION_WORKER_GATE"),
            std::env::var_os("SMELT_GENERATION_WORKER_STARTED"),
            std::env::var_os("SMELT_GENERATION_WORKER_PAYLOAD"),
        ) else {
            return;
        };
        fs::write(&started, b"started").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while !Path::new(&gate).exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "publish gate was not opened"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let store = GenerationStore::new(PathBuf::from(runtime));
        let id = id.to_string_lossy().into_owned();
        let payload = payload.to_string_lossy().into_owned();
        let _manager = store.lock_manager().unwrap();
        store
            .publish_locked(GenerationKind::Bun, &id, &manifest(&id), |staging| {
                fs::write(staging.join("bun"), payload).map_err(|error| error.to_string())?;
                std::thread::sleep(Duration::from_millis(40));
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn concurrent_processes_publish_one_complete_generation() {
        let temp = tempfile::tempdir().unwrap();
        let store = GenerationStore::new(temp.path().join("runtime"));
        let id = "bun-dddddddd";
        let gate = temp.path().join("go");
        let mut children = Vec::new();
        let mut started = Vec::new();
        for payload in ["worker-a", "worker-b"] {
            let marker = temp.path().join(format!("{payload}.started"));
            let child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "runtime_generation::tests::concurrent_publish_worker",
                    "--nocapture",
                ])
                .env("SMELT_GENERATION_WORKER_RUNTIME", store.runtime_root())
                .env("SMELT_GENERATION_WORKER_ID", id)
                .env("SMELT_GENERATION_WORKER_GATE", &gate)
                .env("SMELT_GENERATION_WORKER_STARTED", &marker)
                .env("SMELT_GENERATION_WORKER_PAYLOAD", payload)
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap();
            children.push(child);
            started.push(marker);
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while started.iter().any(|marker| !marker.exists()) {
            assert!(
                std::time::Instant::now() < deadline,
                "publish workers did not reach the barrier"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        fs::write(&gate, b"go").unwrap();
        for child in children {
            assert!(child.wait_with_output().unwrap().status.success());
        }

        let root = store.generation_root(GenerationKind::Bun, id);
        let payload = fs::read(root.join("bun")).unwrap();
        assert!(payload == b"worker-a" || payload == b"worker-b");
        assert!(published_marker_is_valid(&root));
        let parsed: BunGenerationManifest =
            serde_json::from_slice(&fs::read(root.join(MANIFEST_FILE)).unwrap()).unwrap();
        assert_eq!(parsed, manifest(id));
    }

    #[test]
    fn stale_staging_is_removed_without_touching_published_generations() {
        let temp = tempfile::tempdir().unwrap();
        let store = GenerationStore::new(temp.path().join("runtime"));
        let id = "bun-eeeeeeee";
        {
            let _manager = store.lock_manager().unwrap();
            store
                .publish_locked(GenerationKind::Bun, id, &manifest(id), |staging| {
                    fs::write(staging.join("bun"), b"published").map_err(|e| e.to_string())
                })
                .unwrap();
            let stale = store.staging_root().join(".bun-interrupted-build");
            fs::create_dir_all(&stale).unwrap();
            fs::write(stale.join("partial"), b"not ready").unwrap();
            store.remove_stale_staging_locked();
        }
        assert!(!store.staging_root().join(".bun-interrupted-build").exists());
        assert_eq!(
            fs::read(store.generation_root(GenerationKind::Bun, id).join("bun")).unwrap(),
            b"published"
        );
    }

    #[test]
    fn damaged_generation_is_quarantined_only_after_lease_is_exclusive() {
        let temp = tempfile::tempdir().unwrap();
        let store = GenerationStore::new(temp.path().join("runtime"));
        let id = "bun-ffffffff";
        {
            let _manager = store.lock_manager().unwrap();
            store
                .publish_locked(GenerationKind::Bun, id, &manifest(id), |staging| {
                    fs::write(staging.join("bun"), b"published").map_err(|e| e.to_string())
                })
                .unwrap();
        }
        let lease = store.acquire_shared(GenerationKind::Bun, id).unwrap();
        {
            let _manager = store.lock_manager().unwrap();
            assert!(
                store
                    .discard_invalid_locked(GenerationKind::Bun, id)
                    .is_err()
            );
        }
        assert!(store.generation_root(GenerationKind::Bun, id).exists());
        drop(lease);
        {
            let _manager = store.lock_manager().unwrap();
            store
                .discard_invalid_locked(GenerationKind::Bun, id)
                .unwrap();
            assert!(!store.generation_root(GenerationKind::Bun, id).exists());
            assert!(store.lease_path(GenerationKind::Bun, id).exists());
            store
                .publish_locked(GenerationKind::Bun, id, &manifest(id), |staging| {
                    fs::write(staging.join("bun"), b"rebuilt").map_err(|e| e.to_string())
                })
                .unwrap();
        }
        assert_eq!(
            fs::read(store.generation_root(GenerationKind::Bun, id).join("bun")).unwrap(),
            b"rebuilt"
        );
    }

    #[test]
    fn current_switch_is_atomic_and_does_not_delete_old_generations() {
        let temp = tempfile::tempdir().unwrap();
        let store = GenerationStore::new(temp.path().join("runtime"));
        let ids = ["bun-11111111", "bun-22222222"];
        {
            let _manager = store.lock_manager().unwrap();
            for id in ids {
                store
                    .publish_locked(GenerationKind::Bun, id, &manifest(id), |staging| {
                        fs::write(staging.join("bun"), id).map_err(|error| error.to_string())
                    })
                    .unwrap();
            }
            store
                .write_current_locked(GenerationKind::Bun, ids[0])
                .unwrap();
        }
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader_store = store.clone();
        let reader_done = done.clone();
        let reader = std::thread::spawn(move || {
            while !reader_done.load(std::sync::atomic::Ordering::Acquire) {
                let current = reader_store.current_locked(GenerationKind::Bun);
                assert!(current.as_deref().is_some_and(|id| ids.contains(&id)));
            }
        });
        for index in 0..24 {
            let _manager = store.lock_manager().unwrap();
            store
                .write_current_locked(GenerationKind::Bun, ids[index % ids.len()])
                .unwrap();
        }
        done.store(true, std::sync::atomic::Ordering::Release);
        reader.join().unwrap();
        assert!(
            ids.iter()
                .all(|id| store.generation_root(GenerationKind::Bun, id).is_dir())
        );
        assert!(
            store
                .current_locked(GenerationKind::Bun)
                .as_deref()
                .is_some_and(|id| ids.contains(&id))
        );
    }
}
