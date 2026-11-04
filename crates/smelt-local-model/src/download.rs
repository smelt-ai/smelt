use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::sync::watch;

use crate::error::{Error, Result};
use crate::spec::ModelSpec;

const CHUNK: usize = 64 * 1024;
/// 进度回调的最小间隔字节数，避免每个分块都打扰调用方。
const PROGRESS_STEP: u64 = 1024 * 1024;

/// 下载进度。`total` 为模型规格里的固定大小。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadProgress {
    pub downloaded: u64,
    pub total: u64,
}

/// 模型在 `dir` 下的安装路径（不检查是否存在）。
pub fn model_path(dir: &Path, spec: &ModelSpec) -> PathBuf {
    dir.join(spec.file_name)
}

/// 快速判断是否已安装：文件存在且大小正确。
///
/// 安装只经原子发布，发布前已校验 sha256，所以这里不重复整文件哈希；
/// 需要强校验时调用 [`verify_file`]。
pub fn is_installed(dir: &Path, spec: &ModelSpec) -> Result<bool> {
    match fs::metadata(model_path(dir, spec)) {
        Ok(m) => Ok(m.is_file() && m.len() == spec.size),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// 完整校验文件大小与 sha256。
pub fn verify_file(path: &Path, spec: &ModelSpec) -> Result<()> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(Error::NotInstalled(path.to_path_buf()));
        }
        Err(e) => return Err(e.into()),
    };
    let mut reader = BufReader::with_capacity(CHUNK, file);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    let mut size = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        size += n as u64;
    }
    check(spec, size, hasher)
}

/// 卸载：删除已安装的模型文件。不存在视为成功。返回是否真的删了文件。
pub fn remove_model(dir: &Path, spec: &ModelSpec) -> Result<bool> {
    match fs::remove_file(model_path(dir, spec)) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// 下载取消信号。可克隆，跨线程共享；[`CancelToken::cancel`] 会立刻唤醒正在等网络的下载，
/// 不依赖下一个分块到达。
#[derive(Clone, Debug)]
pub struct CancelToken(Arc<watch::Sender<bool>>);

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelToken {
    pub fn new() -> Self {
        Self(Arc::new(watch::Sender::new(false)))
    }

    /// 请求取消；幂等。
    pub fn cancel(&self) {
        self.0.send_replace(true);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }

    /// 取消发生时完成（已取消则立即完成）。
    pub async fn cancelled(&self) {
        let mut rx = self.0.subscribe();
        // Sender 由 self 持有，不会关闭，wait_for 只会在值为 true 时返回。
        let _ = rx.wait_for(|c| *c).await;
    }
}

/// 连接建立的上限。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// 单次读等待的上限：网络停滞超过这么久判定失败。不设整体上限，慢网也能下完；
/// 用户取消由 [`CancelToken`] 事件立即打断，不受这个上限影响。
const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// 从固定 revision 的地址下载模型到 `dir`，校验通过后原子发布，返回安装路径。
///
/// 异步函数，需在 tokio 运行时中执行（含 IO 与 time 驱动）。`cancel` 触发后，
/// 无论是在等连接、等响应头还是等数据，都会立即返回 [`Error::Cancelled`]；
/// 临时文件自动删除，已安装文件不受影响。
pub async fn download_model(
    dir: &Path,
    spec: &ModelSpec,
    progress: &mut dyn FnMut(DownloadProgress),
    cancel: &CancelToken,
) -> Result<PathBuf> {
    download_from_url(&spec.url(), dir, spec, progress, cancel).await
}

async fn download_from_url(
    url: &str,
    dir: &Path,
    spec: &ModelSpec,
    progress: &mut dyn FnMut(DownloadProgress),
    cancel: &CancelToken,
) -> Result<PathBuf> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(Error::Cancelled),
        r = fetch(url, dir, spec, progress, cancel) => r,
    }
}

async fn fetch(
    url: &str,
    dir: &Path,
    spec: &ModelSpec,
    progress: &mut dyn FnMut(DownloadProgress),
    cancel: &CancelToken,
) -> Result<PathBuf> {
    let client = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .build()?;
    let mut resp = client.get(url).send().await?;
    let status = resp.status();
    if !status.is_success() {
        return Err(Error::HttpStatus(status.as_u16()));
    }
    if let Some(len) = resp.content_length()
        && len != spec.size
    {
        return Err(Error::SizeMismatch {
            expected: spec.size,
            actual: len,
        });
    }
    let mut sink = Sink::new(dir, spec, progress)?;
    while let Some(chunk) = resp.chunk().await? {
        sink.push(&chunk, progress)?;
    }
    sink.finish(cancel)
}

/// 把任意字节流作为模型安装：流式写入 `dir` 下的临时文件并计算 sha256，
/// 大小与哈希都对上后 fsync 并原子 rename 到 [`model_path`]。
///
/// 阻塞调用，在每个分块前检查 `cancel`。任何失败（含取消、超长、哈希不符）都会丢弃临时文件。
pub fn install_from_reader(
    mut reader: impl Read,
    dir: &Path,
    spec: &ModelSpec,
    progress: &mut dyn FnMut(DownloadProgress),
    cancel: &CancelToken,
) -> Result<PathBuf> {
    let mut sink = Sink::new(dir, spec, progress)?;
    let mut buf = vec![0u8; CHUNK];
    loop {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        };
        sink.push(&buf[..n], progress)?;
    }
    sink.finish(cancel)
}

/// 流式写入临时文件并校验；drop 未 finish 的 Sink 会删除临时文件。
struct Sink<'a> {
    dir: &'a Path,
    spec: &'a ModelSpec,
    // 临时文件与目标同目录，保证 rename 是同一文件系统内的原子操作。
    tmp: tempfile::NamedTempFile,
    hasher: Sha256,
    written: u64,
    last_report: u64,
}

impl<'a> Sink<'a> {
    fn new(
        dir: &'a Path,
        spec: &'a ModelSpec,
        progress: &mut dyn FnMut(DownloadProgress),
    ) -> Result<Self> {
        fs::create_dir_all(dir)?;
        let tmp = tempfile::Builder::new()
            .prefix(".download-")
            .suffix(".part")
            .tempfile_in(dir)?;
        progress(DownloadProgress {
            downloaded: 0,
            total: spec.size,
        });
        Ok(Self {
            dir,
            spec,
            tmp,
            hasher: Sha256::new(),
            written: 0,
            last_report: 0,
        })
    }

    fn push(&mut self, data: &[u8], progress: &mut dyn FnMut(DownloadProgress)) -> Result<()> {
        self.written += data.len() as u64;
        if self.written > self.spec.size {
            return Err(Error::SizeMismatch {
                expected: self.spec.size,
                actual: self.written,
            });
        }
        self.hasher.update(data);
        self.tmp.write_all(data)?;
        if self.written - self.last_report >= PROGRESS_STEP || self.written == self.spec.size {
            self.last_report = self.written;
            progress(DownloadProgress {
                downloaded: self.written,
                total: self.spec.size,
            });
        }
        Ok(())
    }

    fn finish(self, cancel: &CancelToken) -> Result<PathBuf> {
        check(self.spec, self.written, self.hasher)?;
        self.tmp.as_file().sync_all()?;
        // 发布前最后确认一次：取消后不得改动已安装文件。
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let dest = model_path(self.dir, self.spec);
        self.tmp.persist(&dest).map_err(|e| Error::Io(e.error))?;
        sync_dir(self.dir);
        Ok(dest)
    }
}

fn check(spec: &ModelSpec, size: u64, hasher: Sha256) -> Result<()> {
    if size != spec.size {
        return Err(Error::SizeMismatch {
            expected: spec.size,
            actual: size,
        });
    }
    let actual = to_hex(&hasher.finalize());
    if !actual.eq_ignore_ascii_case(spec.sha256) {
        return Err(Error::ChecksumMismatch {
            expected: spec.sha256.to_owned(),
            actual,
        });
    }
    Ok(())
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// 尽力 fsync 目录，让 rename 持久化；失败不影响结果（部分平台不支持）。
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

#[cfg(test)]
mod tests {
    use super::*;

    // sha256("abc")
    const ABC: ModelSpec = ModelSpec {
        repo: "test/test",
        revision: "0",
        file_name: "abc.gguf",
        size: 3,
        sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    };

    fn cancelled() -> CancelToken {
        let c = CancelToken::new();
        c.cancel();
        c
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn installs_valid_stream_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let mut seen = Vec::new();
        let path = install_from_reader(
            &b"abc"[..],
            dir.path(),
            &ABC,
            &mut |p| seen.push(p),
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(path, model_path(dir.path(), &ABC));
        assert_eq!(fs::read(&path).unwrap(), b"abc");
        assert_eq!(leftovers(dir.path()), vec!["abc.gguf".to_owned()]);
        assert_eq!(
            seen.last(),
            Some(&DownloadProgress {
                downloaded: 3,
                total: 3
            })
        );
        assert!(is_installed(dir.path(), &ABC).unwrap());
        verify_file(&path, &ABC).unwrap();
    }

    #[test]
    fn rejects_bad_hash_without_publishing() {
        let dir = tempfile::tempdir().unwrap();
        let err = install_from_reader(
            &b"abd"[..],
            dir.path(),
            &ABC,
            &mut |_| {},
            &CancelToken::new(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::ChecksumMismatch { .. }));
        assert!(leftovers(dir.path()).is_empty());
        assert!(!is_installed(dir.path(), &ABC).unwrap());
    }

    #[test]
    fn rejects_wrong_size() {
        let dir = tempfile::tempdir().unwrap();
        for data in [&b"ab"[..], &b"abcd"[..]] {
            let err = install_from_reader(
                data,
                dir.path(),
                &ABC,
                &mut |_| {},
                &CancelToken::new(),
            )
            .unwrap_err();
            assert!(matches!(err, Error::SizeMismatch { .. }), "{err}");
        }
        assert!(leftovers(dir.path()).is_empty());
    }

    #[test]
    fn cancel_keeps_existing_install() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(model_path(dir.path(), &ABC), b"abc").unwrap();
        let err = install_from_reader(
            &b"abc"[..],
            dir.path(),
            &ABC,
            &mut |_| {},
            &cancelled(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::Cancelled));
        assert_eq!(leftovers(dir.path()), vec!["abc.gguf".to_owned()]);
    }

    #[test]
    fn verify_and_remove() {
        let dir = tempfile::tempdir().unwrap();
        let path = model_path(dir.path(), &ABC);
        assert!(matches!(
            verify_file(&path, &ABC),
            Err(Error::NotInstalled(_))
        ));
        fs::write(&path, b"xyz").unwrap();
        assert!(matches!(
            verify_file(&path, &ABC),
            Err(Error::ChecksumMismatch { .. })
        ));
        assert!(remove_model(dir.path(), &ABC).unwrap());
        assert!(!remove_model(dir.path(), &ABC).unwrap());
    }

    /// 服务器发完响应头后停止发送数据：取消必须立即打断等待中的读，
    /// 不等读超时，也不动已安装文件。
    #[tokio::test]
    async fn cancel_interrupts_stalled_download() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let dir = tempfile::tempdir().unwrap();
        fs::write(model_path(dir.path(), &ABC), b"old").unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req = [0u8; 1024];
            let _ = sock.read(&mut req).await.unwrap();
            sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\na")
                .await
                .unwrap();
            // 停住连接，直到测试结束。
            let _ = release_rx.await;
        });

        let cancel = CancelToken::new();
        let trigger = cancel.clone();
        let err = download_from_url(
            &format!("http://{addr}/abc.gguf"),
            dir.path(),
            &ABC,
            // 拿到响应头、开始写入时取消，之后的读会永远挂住。
            &mut |_| trigger.cancel(),
            &cancel,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, Error::Cancelled), "{err}");
        assert_eq!(fs::read(model_path(dir.path(), &ABC)).unwrap(), b"old");
        assert_eq!(leftovers(dir.path()), vec!["abc.gguf".to_owned()]);

        let _ = release_tx.send(());
        server.await.unwrap();
    }
}
