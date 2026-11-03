//! 项目文件：逐层浏览、搜索、打开预览。第一版只读。
//!
//! 文件在运行时那台机器上，没有推送：页面打开和下拉刷新时取一次。文本的正文留在 Rust，
//! Dart 拿 [`TextDocument`] 按行取，带语法高亮的类别，颜色由 Dart 的主题给。

use std::sync::Arc;

use flutter_rust_bridge::frb;

use crate::api::error::SmeltError;

/// 目录里的一项。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub name: String,
    /// 那台机器上的绝对路径。
    pub path: String,
    pub is_dir: bool,
}

/// 一条搜索命中：文件名命中没有行；内容命中带行号和那一行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSearchHit {
    pub path: String,
    /// 相对项目根的路径，给人看。
    pub rel: String,
    /// 1 基行号。
    pub line: Option<u32>,
    pub preview: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSearch {
    pub hits: Vec<FileSearchHit>,
    /// 命中太多被截断了。
    pub truncated: bool,
}

/// 一段文字该用什么颜色。按语义分，不按语言分；颜色在 Dart 的主题里。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokenKind {
    Plain,
    Comment,
    String,
    Number,
    Constant,
    Keyword,
    Operator,
    Type,
    Function,
    Variable,
    Tag,
    Attribute,
    Punctuation,
    Heading,
    Link,
    Emphasis,
    Inserted,
    Deleted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenSpan {
    pub text: String,
    pub kind: TokenKind,
}

/// 一行。空行没有片段。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HighlightedLine {
    /// 1 基行号。
    pub number: u32,
    pub spans: Vec<TokenSpan>,
}

/// 一份打开着的文本文件。正文留在 Rust，按行取；Dart 不再用时自动释放。
#[frb(opaque)]
pub struct TextDocument {
    inner: Arc<crate::highlight::Document>,
}

impl TextDocument {
    pub(crate) fn new(document: crate::highlight::Document) -> Self {
        Self {
            inner: Arc::new(document),
        }
    }

    #[frb(sync, getter)]
    pub fn line_count(&self) -> u32 {
        self.inner.line_count() as u32
    }

    /// 最长一行有几个字。不换行显示时据此定横向宽度。
    #[frb(sync, getter)]
    pub fn max_line_chars(&self) -> u32 {
        self.inner.max_line_chars() as u32
    }

    #[frb(sync, getter)]
    pub fn byte_len(&self) -> u64 {
        self.inner.byte_len() as u64
    }

    /// 认出来的语言名；认不出是空。
    #[frb(sync, getter)]
    pub fn language(&self) -> Option<String> {
        self.inner.language().map(str::to_string)
    }

    /// 会不会上色。文件太大、有超长行时即使认出了语言也不上色。
    #[frb(sync, getter)]
    pub fn highlighted(&self) -> bool {
        self.inner.highlighted()
    }

    /// `[start, end)` 行，0 基。在后台线程解析，跳到很靠后的位置时要等一会儿。
    pub fn lines(&self, start: u32, end: u32) -> Vec<HighlightedLine> {
        self.inner.lines(start as usize, end as usize)
    }

    /// 全文：Markdown 渲染、复制全文用。
    pub fn full_text(&self) -> String {
        self.inner.text().to_string()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilePreviewKind {
    /// `document`。
    Text,
    /// `image`、`svg`。
    Image,
    /// 不是 UTF-8 文本，也不是认得的图片，没有内容。
    Binary,
    /// 超过运行时肯读的大小：`size`、`limit`，没有内容。
    TooLarge,
}

/// 一个文件的预览。哪些字段有值看 `kind`。
pub struct FilePreview {
    pub kind: FilePreviewKind,
    pub document: Option<TextDocument>,
    /// 图片原始字节。
    pub image: Option<Vec<u8>>,
    /// 图片是 SVG，要交给 SVG 渲染。
    pub svg: bool,
    pub size: Option<u64>,
    pub limit: Option<u64>,
}

impl FilePreview {
    fn empty(kind: FilePreviewKind) -> Self {
        Self {
            kind,
            document: None,
            image: None,
            svg: false,
            size: None,
            limit: None,
        }
    }

    pub(crate) fn text(document: TextDocument) -> Self {
        Self {
            document: Some(document),
            ..Self::empty(FilePreviewKind::Text)
        }
    }

    pub(crate) fn image(bytes: Vec<u8>, svg: bool) -> Self {
        Self {
            image: Some(bytes),
            svg,
            ..Self::empty(FilePreviewKind::Image)
        }
    }

    pub(crate) fn binary() -> Self {
        Self::empty(FilePreviewKind::Binary)
    }

    pub(crate) fn too_large(size: u64, limit: u64) -> Self {
        Self {
            size: Some(size),
            limit: Some(limit),
            ..Self::empty(FilePreviewKind::TooLarge)
        }
    }
}

/// 列项目里的一层：目录在前，按名字排。`.git`、`node_modules` 这类由运行时滤掉。
/// `dir` 必须在 `root` 里面。
pub fn list_project_dir(
    runtime_id: String,
    root: String,
    dir: String,
) -> Result<Vec<FileEntry>, SmeltError> {
    let node = crate::fleet::fleet().node(&runtime_id)?;
    let style = node.catalog.path_style();
    node.files.list_dir(&root, &dir, style)
}

/// 按文件名和内容搜，不区分大小写，遵守 `.gitignore`。
pub fn search_project(
    runtime_id: String,
    root: String,
    query: String,
) -> Result<FileSearch, SmeltError> {
    crate::fleet::fleet()
        .node(&runtime_id)?
        .files
        .search(&root, &query)
}

/// 打开一个文件预览。路径不限于项目里：对话里提到的文件可能在别处。
pub fn open_file(runtime_id: String, path: String) -> Result<FilePreview, SmeltError> {
    crate::fleet::fleet().node(&runtime_id)?.files.open(&path)
}

/// 对话里 Markdown 链接的目标是不是那台机器上的文件。是的话给出绝对路径和行号，
/// 规则和桌面一致（`smelt_model::file_link`）。
#[frb(sync)]
pub fn resolve_file_link(
    runtime_id: String,
    cwd: Option<String>,
    target: String,
) -> Option<FileLinkTarget> {
    let style = crate::fleet::fleet()
        .node(&runtime_id)
        .map(|node| node.catalog.cached_path_style())
        .unwrap_or(smelt_model::runtime_path::PathStyle::Posix);
    let link = smelt_model::file_link::parse_file_link(&target, cwd.as_deref(), style)?;
    Some(FileLinkTarget {
        line: link.line(),
        path: link.path,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileLinkTarget {
    pub path: String,
    /// 1 基行号。
    pub line: Option<u32>,
}
