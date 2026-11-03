//! 一台机器上的项目文件：列目录、搜索、打开预览。Dart 入口见 `crate::api::files`。
//!
//! 全是一问一答，不缓存：文件随时在变，手机上看文件是短时、主动的行为，进页面和下拉
//! 刷新时重取即可，不订阅 `project.changed`。

#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::time::Duration;

use smelt_model::runtime_path::PathStyle;
use smelt_runtime_client::protocol::files::{
    FileContent, FileOpenCall, FilePathParams, ProjectDirParams, ProjectSearchCall,
    ProjectSearchParams, ProjectTreeCall,
};

use crate::api::error::{SmeltError, SmeltErrorKind, error};
use crate::api::files::{FileEntry, FilePreview, FileSearch, FileSearchHit, TextDocument};
use crate::catalog::control_error;
use crate::connection::CurrentRuntime;

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
}

impl Files {
    pub(crate) fn new(source: Arc<dyn RuntimeSource>, timing: Timing) -> Self {
        Self { source, timing }
    }

    fn runtime(&self) -> Result<CurrentRuntime, SmeltError> {
        self.source
            .current()
            .ok_or_else(|| error(SmeltErrorKind::Offline, "还没有连上这台机器"))
    }

    /// 列一层。`style` 是这台机器的路径规则，用来拼子项的绝对路径。
    pub(crate) fn list_dir(
        &self,
        root: &str,
        dir: &str,
        style: PathStyle,
    ) -> Result<Vec<FileEntry>, SmeltError> {
        let tree = self
            .runtime()?
            .client
            .call::<ProjectTreeCall>(
                &ProjectDirParams {
                    root: root.to_string(),
                    dir: dir.to_string(),
                },
                self.timing.call_timeout,
            )
            .map_err(control_error)?;
        Ok(tree
            .entries
            .into_iter()
            .map(|entry| FileEntry {
                path: style.join(dir, &entry.name),
                name: entry.name,
                is_dir: entry.is_dir,
            })
            .collect())
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

    /// 打开预览。文本交给高亮器，正文留在 Rust。
    pub(crate) fn open(&self, path: &str) -> Result<FilePreview, SmeltError> {
        let opened = self
            .runtime()?
            .client
            .call::<FileOpenCall>(
                &FilePathParams {
                    path: path.to_string(),
                },
                self.timing.open_timeout,
            )
            .map_err(control_error)?;
        Ok(match opened.content {
            FileContent::Text { text, .. } => FilePreview::text(TextDocument::new(
                crate::highlight::Document::new(path, text),
            )),
            FileContent::Image { data } => FilePreview::image(data.0, is_svg(path)),
            FileContent::Binary => FilePreview::binary(),
            FileContent::TooLarge { size, limit } => FilePreview::too_large(size, limit),
        })
    }
}

fn is_svg(path: &str) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("svg"))
}
