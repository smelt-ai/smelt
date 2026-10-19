//! 项目文件和文件查看器的业务方法。
//!
//! 没有通用的读写原语（见设计文档 D4）：浏览、删除、搜索以项目为范围，运行时拒绝
//! 越出项目根的路径；查看器按路径打开和保存，因为要打开的文件可能来自终端里的
//! `smelt open`、对话中的链接，不一定在任何项目里。大小上限、二进制识别、保存冲突
//! 这些规则都在运行时里执行。

use serde::{Deserialize, Serialize};

use super::control::{ControlCall, ControlMethod, EmptyParams};

macro_rules! file_calls {
    ($( $(#[$doc:meta])* $call:ident => $method:ident ($params:ty) -> $result:ty; )+) => {
        $(
            $(#[$doc])*
            pub enum $call {}

            impl ControlCall for $call {
                type Params = $params;
                type Result = $result;

                const METHOD: ControlMethod = ControlMethod::$method;
            }
        )+
    };
}

file_calls! {
    /// 项目里一个目录的直接子项。遵守噪音目录过滤，目录在前，按名字排序。
    ProjectTreeCall => ProjectTree(ProjectDirParams) -> ProjectTreeResult;
    /// 按文件名和内容搜索项目，遵守 ignore 规则，命中数有上限。
    ProjectSearchCall => ProjectSearch(ProjectSearchParams) -> ProjectSearchResult;
    /// 删除项目里的一个文件或目录（目录连同内容）。不能删项目根本身。
    ProjectDeleteEntryCall => ProjectDeleteEntry(ProjectPathParams) -> EmptyParams;
    /// 打开文件查看或编辑。超过大小上限、不是 UTF-8 文本的，只说明原因不给内容。
    FileOpenCall => FileOpen(FilePathParams) -> FileOpenResult;
    /// 保存。`base_version` 和磁盘当前内容对不上时返回 `conflict`，不写入。
    FileSaveCall => FileSave(FileSaveParams) -> FileSaveResult;
    /// 会话输入框 `@` 补全的候选。一次取全，客户端在本地按输入过滤排序，打字不用等往返。
    SessionCompleteFilesCall => SessionCompleteFiles(CompleteFilesParams) -> CompleteFilesResult;
    /// 关注一个项目：之后它的文件有变化，运行时发 `project.changed` 事件。幂等。
    ProjectWatchCall => ProjectWatch(ProjectRootParams) -> EmptyParams;
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRootParams {
    /// 项目根，绝对路径。
    pub root: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectDirParams {
    /// 项目根，绝对路径。
    pub root: String,
    /// 要列的目录，绝对路径，必须在 `root` 之内（可以就是 `root`）。
    pub dir: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectEntry {
    pub name: String,
    pub is_dir: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectTreeResult {
    pub entries: Vec<ProjectEntry>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSearchParams {
    pub root: String,
    /// 大小写不敏感。
    pub query: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSearchHit {
    /// 命中文件的绝对路径。
    pub path: String,
    /// 相对项目根的展示路径。
    pub rel: String,
    /// 内容命中时的首个匹配行：(行号从 1 起, 该行预览)；文件名命中为 `None`。
    pub line: Option<(usize, String)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSearchResult {
    /// 文件名命中在前、内容命中在后。
    pub hits: Vec<ProjectSearchHit>,
    /// 命中数触顶后停止，后面还有。
    pub truncated: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectPathParams {
    pub root: String,
    /// 绝对路径，必须在 `root` 之内，且不能是 `root` 本身。
    pub path: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilePathParams {
    /// 运行时那台机器上的绝对路径。
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileContent {
    Text {
        text: String,
        /// 内容版本，保存时原样带回来做冲突检测。客户端不解析它。
        version: String,
    },
    /// 查看器能预览的图片（见 [`is_previewable_image`]），原始字节。
    Image { data: Base64Bytes },
    /// 不是 UTF-8 文本。
    Binary,
    /// 超过查看器的大小上限。
    TooLarge { size: u64, limit: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileOpenResult {
    pub content: FileContent,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSaveParams {
    pub path: String,
    pub text: String,
    /// 打开或上次保存时拿到的版本。`None` 表示用户已确认覆盖，跳过冲突检测。
    pub base_version: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSaveResult {
    /// 写入后的新版本。
    pub version: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompleteFilesParams {
    /// 会话的工作目录。
    pub cwd: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompleteFilesResult {
    /// 相对 `cwd` 的文件，以及它们的上级目录（目录以 `/` 结尾），已排序。
    pub entries: Vec<String>,
    /// 超过上限被截断。
    pub truncated: bool,
}

/// 查看器能直接预览的图片格式。ICNS 不在客户端的图片解码格式里，仍按二进制处理，
/// 免得显示一个加载失败的空白画布。运行时据此决定 `file.open` 返回图片还是文本。
pub const PREVIEWABLE_IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "webp", "gif", "bmp", "tif", "tiff", "svg",
];

pub fn is_previewable_image(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            PREVIEWABLE_IMAGE_EXTENSIONS
                .iter()
                .any(|known| ext.eq_ignore_ascii_case(known))
        })
}

/// 二进制内容在 JSON 里按 base64 字符串传。直接序列化 `Vec<u8>` 会变成数字数组，
/// 体积是原来的三四倍。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Base64Bytes(pub Vec<u8>);

impl Serialize for Base64Bytes {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use base64::Engine as _;
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for Base64Bytes {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use base64::Engine as _;
        let text = String::deserialize(deserializer)?;
        base64::engine::general_purpose::STANDARD
            .decode(text)
            .map(Base64Bytes)
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_bytes_travel_as_base64_text() {
        let content = FileContent::Image {
            data: Base64Bytes(vec![0x89, b'P', b'N', b'G']),
        };
        let wire = serde_json::to_value(&content).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({ "kind": "image", "data": "iVBORw==" })
        );
        assert_eq!(
            serde_json::from_value::<FileContent>(wire).unwrap(),
            content
        );
    }

    #[test]
    fn previewable_images_are_recognised_case_insensitively() {
        assert!(is_previewable_image("/a/shot.PNG"));
        assert!(is_previewable_image("/a/logo.svg"));
        assert!(!is_previewable_image("/a/app.icns"));
        assert!(!is_previewable_image("/a/README"));
    }
}
