//! 一台机器上的项目文件：列目录、搜索、打开预览。Dart 入口见 `crate::api::files`。
//!
//! 列目录和搜索一问一答、不缓存；打开预览保留有界内存 LRU 缓存并带上 `if_none_match`
//! 走条件校验，新开文件或关闭节点时会通过 `CallCancel` 中止在途打开请求（不订阅
//! `project.changed`）。

#[cfg(test)]
mod tests;

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rust_i18n::t;
use smelt_model::runtime_path::PathStyle;
use smelt_runtime_client::CallCancel;
use smelt_runtime_client::protocol::control::ControlClientError;
use smelt_runtime_client::protocol::files::{
    FileContent, FileOpenCall, FilePathParams, ProjectSearchCall, ProjectSearchParams,
};
use smelt_runtime_client::protocol::fs::{FsKind, FsReadDirCall};

use crate::api::error::{SmeltError, SmeltErrorKind, error};
use crate::api::files::{FileEntry, FilePreview, FileSearch, FileSearchHit, TextDocument};
use crate::catalog::control_error;
use crate::connection::CurrentRuntime;

/// 文件页不显示的目录项。
const HIDDEN_ENTRIES: [&str; 4] = [".git", "node_modules", "target", ".DS_Store"];

/// 这台机器的缓存总字节上限（16 MB），单文件超过上限一半时跳过缓存。
const MAX_CACHE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone)]
enum CachedFile {
    Text {
        document: Arc<crate::highlight::Document>,
        version: String,
    },
    Image {
        bytes: Arc<Vec<u8>>,
        svg: bool,
        version: String,
    },
}

impl CachedFile {
    fn version(&self) -> &str {
        match self {
            Self::Text { version, .. } | Self::Image { version, .. } => version,
        }
    }

    fn byte_len(&self) -> usize {
        match self {
            Self::Text { document, .. } => document.byte_len(),
            Self::Image { bytes, .. } => bytes.len(),
        }
    }
}

struct CachedEntry {
    value: CachedFile,
    bytes: usize,
}

struct FileCache {
    entries: HashMap<String, CachedEntry>,
    order: VecDeque<String>,
    total_bytes: usize,
    max_bytes: usize,
}

impl FileCache {
    fn with_max_bytes(max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            total_bytes: 0,
            max_bytes,
        }
    }

    fn get(&mut self, path: &str) -> Option<CachedFile> {
        let value = self.entries.get(path)?.value.clone();
        self.order.retain(|existing| existing != path);
        self.order.push_back(path.to_string());
        Some(value)
    }

    fn put(&mut self, path: String, value: CachedFile) {
        self.remove(&path);
        let bytes = value.byte_len();
        if bytes > self.max_bytes / 2 {
            return;
        }
        self.entries
            .insert(path.clone(), CachedEntry { value, bytes });
        self.order.push_back(path);
        self.total_bytes += bytes;
        while self.total_bytes > self.max_bytes {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(removed) = self.entries.remove(&oldest) {
                self.total_bytes -= removed.bytes;
            }
        }
    }

    fn remove(&mut self, path: &str) {
        if let Some(removed) = self.entries.remove(path) {
            self.total_bytes -= removed.bytes;
            self.order.retain(|existing| existing != path);
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.total_bytes = 0;
    }
}

struct InflightOpenGuard<'a> {
    slot: &'a Mutex<Option<(u64, CallCancel)>>,
    id: u64,
}

impl Drop for InflightOpenGuard<'_> {
    fn drop(&mut self) {
        let mut guard = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        if guard.as_ref().is_some_and(|(id, _)| *id == self.id) {
            *guard = None;
        }
    }
}

/// 这台机器的连接从哪来。生产用节点的 `NodeSource`（见 `crate::fleet`），测试注入假的。
pub(crate) trait RuntimeSource: Send + Sync {
    fn current(&self) -> Option<CurrentRuntime>;
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Timing {
    /// 列目录、搜索。搜索要遍历整个项目。
    pub call_timeout: Duration,
    /// 打开文件：图片最大 20 MB，经 iroh 中继传过来要一会儿。
    pub open_timeout: Duration,
}

impl Timing {
    pub(crate) const PRODUCTION: Self = Self {
        call_timeout: Duration::from_secs(30),
        open_timeout: Duration::from_secs(120),
    };
}

pub(crate) struct Files {
    source: Arc<dyn RuntimeSource>,
    timing: Timing,
    cache: Mutex<FileCache>,
    inflight_open: Mutex<Option<(u64, CallCancel)>>,
    next_open_id: AtomicU64,
}

impl Files {
    pub(crate) fn new(source: Arc<dyn RuntimeSource>, timing: Timing) -> Self {
        Self::with_cache_limit(source, timing, MAX_CACHE_BYTES)
    }

    pub(crate) fn with_cache_limit(
        source: Arc<dyn RuntimeSource>,
        timing: Timing,
        max_cache_bytes: usize,
    ) -> Self {
        Self {
            source,
            timing,
            cache: Mutex::new(FileCache::with_max_bytes(max_cache_bytes)),
            inflight_open: Mutex::new(None),
            next_open_id: AtomicU64::new(1),
        }
    }

    pub(crate) fn close(&self) {
        let inflight = {
            let mut guard = self.inflight_open.lock().unwrap_or_else(|e| e.into_inner());
            guard.take().map(|(_, cancel)| cancel)
        };
        if let Some(cancel) = inflight {
            cancel.cancel();
        }
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    fn runtime(&self) -> Result<CurrentRuntime, SmeltError> {
        self.source
            .current()
            .ok_or_else(|| error(SmeltErrorKind::Offline, t!("error.not_connected")))
    }

    /// 列一层：滤掉 [`HIDDEN_ENTRIES`]，目录在前、同类按名字不区分大小写排，指向目录的
    /// 符号链接当目录。`style` 是这台机器的路径规则，用来拼子项的绝对路径。
    pub(crate) fn list_dir(
        &self,
        dir: &str,
        style: PathStyle,
    ) -> Result<Vec<FileEntry>, SmeltError> {
        let listed = self
            .runtime()?
            .client
            .call::<FsReadDirCall>(&FilePathParams::new(dir), self.timing.call_timeout)
            .map_err(control_error)?;
        let mut entries: Vec<FileEntry> = listed
            .entries
            .into_iter()
            .filter(|entry| !HIDDEN_ENTRIES.contains(&entry.name.as_str()))
            .map(|entry| {
                let kind = match entry.stat.kind {
                    FsKind::Symlink => entry.stat.target_kind,
                    kind => Some(kind),
                };
                FileEntry {
                    path: style.join(dir, &entry.name),
                    name: entry.name,
                    is_dir: kind == Some(FsKind::Dir),
                }
            })
            .collect();
        entries.sort_by_cached_key(|entry| (!entry.is_dir, entry.name.to_lowercase()));
        Ok(entries)
    }

    pub(crate) fn search(&self, root: &str, query: &str) -> Result<FileSearch, SmeltError> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(FileSearch {
                hits: Vec::new(),
                truncated: false,
            });
        }
        let found = self
            .runtime()?
            .client
            .call::<ProjectSearchCall>(
                &ProjectSearchParams {
                    root: root.to_string(),
                    query: query.to_string(),
                },
                self.timing.call_timeout,
            )
            .map_err(control_error)?;
        Ok(FileSearch {
            hits: found
                .hits
                .into_iter()
                .map(|hit| {
                    let (line, preview) = match hit.line {
                        Some((line, preview)) => (Some(line as u32), Some(preview)),
                        None => (None, None),
                    };
                    FileSearchHit {
                        path: hit.path,
                        rel: hit.rel,
                        line,
                        preview,
                    }
                })
                .collect(),
            truncated: found.truncated,
        })
    }

    /// 打开预览。命中缓存带 `if_none_match` 校验，未变直接复用本地文档或图片字节；
    /// 文本交给高亮器，正文留在 Rust。新发起打开会自动取消前一个未完成的远程拉取。
    pub(crate) fn open(&self, path: &str) -> Result<FilePreview, SmeltError> {
        let runtime = self.runtime()?;
        let id = self.next_open_id.fetch_add(1, Ordering::Relaxed);
        let cancel = CallCancel::new();
        let previous = {
            let mut guard = self.inflight_open.lock().unwrap_or_else(|e| e.into_inner());
            guard.replace((id, cancel.clone())).map(|(_, prev)| prev)
        };
        if let Some(prev) = previous {
            prev.cancel();
        }
        let _inflight_guard = InflightOpenGuard {
            slot: &self.inflight_open,
            id,
        };

        let cached = self
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(path);
        let if_none_match = cached.as_ref().map(|entry| entry.version().to_string());
        let params = FilePathParams {
            path: path.to_string(),
            if_none_match,
        };
        let opened = match runtime.client.call_cancellable::<FileOpenCall>(
            &params,
            self.timing.open_timeout,
            &cancel,
        ) {
            Ok(opened) => opened,
            Err(err) => {
                if matches!(err, ControlClientError::Protocol(_)) {
                    self.cache
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(path);
                }
                return Err(control_error(err));
            }
        };

        Ok(match opened.content {
            FileContent::Text { text, version } => {
                let document = Arc::new(crate::highlight::Document::new(path, text));
                if !cancel.is_cancelled() {
                    self.cache.lock().unwrap_or_else(|e| e.into_inner()).put(
                        path.to_string(),
                        CachedFile::Text {
                            document: Arc::clone(&document),
                            version,
                        },
                    );
                }
                FilePreview::text(TextDocument::from_arc(document))
            }
            FileContent::Image { data, version } => {
                let svg = is_svg(path);
                if !cancel.is_cancelled() {
                    let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
                    match version {
                        Some(version) => cache.put(
                            path.to_string(),
                            CachedFile::Image {
                                bytes: Arc::new(data.0.clone()),
                                svg,
                                version,
                            },
                        ),
                        None => cache.remove(path),
                    }
                }
                FilePreview::image(data.0, svg)
            }
            FileContent::NotModified { .. } => match cached {
                Some(CachedFile::Text { document, .. }) => {
                    FilePreview::text(TextDocument::from_arc(document))
                }
                Some(CachedFile::Image { bytes, svg, .. }) => {
                    FilePreview::image((*bytes).clone(), svg)
                }
                None => {
                    return Err(control_error(ControlClientError::InvalidResponse(
                        "unexpected not_modified without cached entry".into(),
                    )));
                }
            },
            FileContent::Binary => {
                self.cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(path);
                FilePreview::binary()
            }
            FileContent::TooLarge { size, limit } => {
                self.cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(path);
                FilePreview::too_large(size, limit)
            }
        })
    }
}

fn is_svg(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    name.rsplit_once('.')
        .filter(|(stem, _)| !stem.is_empty())
        .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("svg"))
}
