//! 项目和文件查看器的业务方法。
//!
//! 搜索、监听以项目为范围（要按 ignore 规则、滤掉构建产物）；查看器按路径打开和保存，
//! 大小上限、二进制识别、保存冲突这些规则都在运行时里执行。列目录、删除这些通用操作
//! 见 [`super::fs`]。

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
    /// 按文件名和内容搜索项目，遵守 ignore 规则，命中数有上限。
    ProjectSearchCall => ProjectSearch(ProjectSearchParams) -> ProjectSearchResult;
    /// 打开文件查看或编辑。超过大小上限、不是 UTF-8 文本的，只说明原因不给内容。
    FileOpenCall => FileOpen(FilePathParams) -> FileOpenResult;
    /// 保存。`base_version` 和磁盘当前内容对不上时返回 `conflict`，不写入。
    FileSaveCall => FileSave(FileSaveParams) -> FileSaveResult;
    /// 会话输入框 `@` 补全的候选。一次取全，客户端在本地按输入过滤排序，打字不用等往返。
    SessionCompleteFilesCall => SessionCompleteFiles(CompleteFilesParams) -> CompleteFilesResult;
    /// 关注一个项目：之后它的文件有变化，运行时发 `project.changed` 事件。幂等。
    ProjectWatchCall => ProjectWatch(ProjectRootParams) -> EmptyParams;
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProjectRootParams {
    /// 项目根，绝对路径。
    pub root: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProjectSearchParams {
    pub root: String,
    /// 大小写不敏感。
    pub query: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProjectSearchHit {
    /// 命中文件的绝对路径。
    pub path: String,
    /// 相对项目根的展示路径。
    pub rel: String,
    /// 内容命中时的首个匹配行：(行号从 1 起, 该行预览)；文件名命中为 `None`。
    pub line: Option<(usize, String)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProjectSearchResult {
    /// 文件名命中在前、内容命中在后。
    pub hits: Vec<ProjectSearchHit>,
    /// 命中数触顶后停止，后面还有。
    pub truncated: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FilePathParams {
    /// 运行时那台机器上的绝对路径。
    pub path: String,
    /// 可选内容版本（sha256）；命中磁盘当前版本时服务端只回 [`FileContent::NotModified`]。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_none_match: Option<String>,
}

impl FilePathParams {
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            if_none_match: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileContent {
    Text {
        text: String,
        /// 内容版本，保存时原样带回来做冲突检测。客户端不解析它。
        version: String,
    },
    /// 查看器能预览的图片（见 [`is_previewable_image`]），原始字节。
    Image {
        data: Base64Bytes,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
    },
    /// 带 `if_none_match` 时磁盘内容未变化，不重复传正文。
    NotModified { version: String },
    /// 不是 UTF-8 文本。
    Binary,
    /// 超过查看器的大小上限。
    TooLarge { size: u64, limit: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FileOpenResult {
    pub content: FileContent,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FileSaveParams {
    pub path: String,
    pub text: String,
    /// 打开或上次保存时拿到的版本。`None` 表示用户已确认覆盖，跳过冲突检测。
    pub base_version: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FileSaveResult {
    /// 写入后的新版本。
    pub version: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CompleteFilesParams {
    /// 会话的工作目录。
    pub cwd: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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
    // 扩展名按运行时路径取，不用 `std::path`（同时认 POSIX `/` 与 Windows `\` 分隔符）。
    // 末尾带分隔符的是目录路径，不认作图片文件。
    if path.ends_with(['/', '\\']) {
        return false;
    }
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return false;
    };
    if stem.is_empty() {
        return false;
    }
    PREVIEWABLE_IMAGE_EXTENSIONS
        .iter()
        .any(|known| ext.eq_ignore_ascii_case(known))
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

impl schemars::JsonSchema for Base64Bytes {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Base64Bytes".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "string", "contentEncoding": "base64" })
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
            version: None,
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

        let with_version = FileContent::Image {
            data: Base64Bytes(vec![0x89, b'P', b'N', b'G']),
            version: Some("abc".into()),
        };
        let wire = serde_json::to_value(&with_version).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({ "kind": "image", "data": "iVBORw==", "version": "abc" })
        );
        assert_eq!(
            serde_json::from_value::<FileContent>(wire).unwrap(),
            with_version
        );
    }

    #[test]
    fn conditional_file_open_wire_round_trips() {
        let bare = FilePathParams::new("/a/b.md");
        assert_eq!(
            serde_json::to_value(&bare).unwrap(),
            serde_json::json!({ "path": "/a/b.md" })
        );
        assert_eq!(
            serde_json::from_value::<FilePathParams>(serde_json::json!({ "path": "/a/b.md" }))
                .unwrap(),
            bare
        );

        let cond = FilePathParams {
            path: "/a/b.md".into(),
            if_none_match: Some("deadbeef".into()),
        };
        let wire = serde_json::to_value(&cond).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({ "path": "/a/b.md", "if_none_match": "deadbeef" })
        );
        assert_eq!(
            serde_json::from_value::<FilePathParams>(wire).unwrap(),
            cond
        );

        let not_modified = FileContent::NotModified {
            version: "deadbeef".into(),
        };
        let wire = serde_json::to_value(&not_modified).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({ "kind": "not_modified", "version": "deadbeef" })
        );
        assert_eq!(
            serde_json::from_value::<FileContent>(wire).unwrap(),
            not_modified
        );
    }

    #[test]
    fn previewable_images_are_recognised_case_insensitively() {
        assert!(is_previewable_image("/a/shot.PNG"));
        assert!(is_previewable_image("/a/logo.svg"));
        assert!(is_previewable_image(r"E:\AI\shot.png"));
        assert!(!is_previewable_image(r"E:\AI\.png"));
        assert!(!is_previewable_image(r"E:\dir.png\"));
        assert!(!is_previewable_image("/a/app.icns"));
        assert!(!is_previewable_image("/a/README"));
    }
}
