//! 工作台布局文件：gpui-base 官方的 `DockAreaState`，原样交给 `DockArea::load`。
//!
//! 格式、面板名、放哪、怎么读见 `docs/workspace-layouts.md`。这里只解析、找文件、
//! 算指纹、读存档；不另加校验，结构由 Dock 收拾，没注册的面板名变占位。
//! 读文件的函数只在启动、命令这类事件里调，不在 render 里。

use std::path::{Path, PathBuf};

use gpui_component::dock::DockAreaState;
use sha2::{Digest as _, Sha256};
use smelt_client_store::LayoutStateRecord;

/// 内置默认布局的 id。用户目录里叫这个名字的文件不加载。
pub(crate) const DEFAULT_ID: &str = "default";
/// 用户和插件放布局文件的目录名。
pub(crate) const LAYOUT_DIR: &str = "layouts";
/// 布局文件和存档的 `version`。面板的数据结构不兼容地变了才加。
pub(crate) const VERSION: usize = 1;
/// 程序里嵌的默认布局。
pub(crate) const DEFAULT_LAYOUT: &str = include_str!("layout_files/default.json");

/// 内置面板的注册名。
pub(crate) mod panel {
    pub(crate) const SESSIONS: &str = "smelt.sessions";
    pub(crate) const STAGE: &str = "smelt.stage";
    pub(crate) const FILES: &str = "smelt.files";
    pub(crate) const GIT: &str = "smelt.git";
    pub(crate) const HISTORY: &str = "smelt.history";
}

/// 读出来的一份布局文件。
#[derive(Clone, Debug)]
pub(crate) struct LayoutFile {
    pub(crate) state: DockAreaState,
    /// 文件内容的指纹。存档记着它，文件改过就不用旧存档。
    pub(crate) digest: String,
}

impl LayoutFile {
    /// 解析布局文件的内容。只认官方格式和我们的 `version`。
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        Ok(Self {
            state: parse_state(text)?,
            digest: digest(text),
        })
    }

    /// 内置默认布局。
    pub(crate) fn default_layout() -> Self {
        Self::parse(DEFAULT_LAYOUT).expect("内置默认布局是合法的")
    }
}

/// 按官方格式解析，`version` 要是 [`VERSION`]。布局文件和存档共用。
fn parse_state(text: &str) -> Result<DockAreaState, String> {
    let state: DockAreaState =
        serde_json::from_str(text).map_err(|error| format!("不是合法的布局：{error}"))?;
    match state.version {
        Some(VERSION) => Ok(state),
        Some(version) => Err(format!("`version` 是 {version}，这个版本只认 {VERSION}")),
        None => Err(format!("没写 `version`，要写 {VERSION}")),
    }
}

/// 布局内容的指纹：SHA-256 的十六进制。
fn digest(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// 存档写进库里的文本，也是 `layout.copy` 导出的文件内容。
pub(crate) fn to_text(state: &DockAreaState) -> String {
    serde_json::to_string_pretty(state).expect("DockAreaState 总能写成 JSON")
}

/// 这份存档能不能用在这份文件上：指纹对得上、内容是当前版本的布局。不能用返回 None，
/// 用文件。
pub(crate) fn saved_state(record: &LayoutStateRecord, file: &LayoutFile) -> Option<DockAreaState> {
    if record.file_digest != file.digest {
        return None;
    }
    match parse_state(&record.state) {
        Ok(state) => Some(state),
        Err(error) => {
            eprintln!("[layout] 布局 `{}` 的存档不用：{error}", record.layout_id);
            None
        }
    }
}

// ---- 布局目录 ----

/// 用户布局的目录：`~/.smelt/layouts/`。
pub(crate) fn user_layout_dir() -> Option<PathBuf> {
    smelt_paths::smelt_home().map(|home| home.join(LAYOUT_DIR))
}

/// 目录里的一个布局文件。不合法的也列出来，带上原因。
#[derive(Clone, Debug)]
pub(crate) struct LayoutEntry {
    /// 用户布局是文件名去掉 `.json`；插件带的是 `<插件 id>/<文件名>`。
    pub(crate) id: String,
    pub(crate) path: PathBuf,
    pub(crate) layout: Result<LayoutFile, String>,
}

/// 读这个目录里的 `*.json`，结果按 id 排序。目录不存在就是没有布局。
pub(crate) fn discover(dir: &Path) -> Vec<LayoutEntry> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<LayoutEntry> = read
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json") && path.is_file())
        .filter_map(|path| {
            let id = path.file_stem()?.to_str()?.to_string();
            let layout = if id == DEFAULT_ID {
                Err(format!("`{DEFAULT_ID}` 留给内置的默认布局，换个文件名"))
            } else {
                read_layout(&path)
            };
            Some(LayoutEntry { id, path, layout })
        })
        .collect();
    entries.sort_by(|a, b| a.id.cmp(&b.id));
    entries
}

/// 插件带的布局：插件目录下 `layouts/` 里的 `*.json`。id 是 `<插件 id>/<文件名>`，
/// 和用户布局（id 就是文件名，不会带 `/`）、别的插件的布局都撞不上。
pub(crate) fn discover_plugin(plugin: &str, plugin_root: &Path) -> Vec<LayoutEntry> {
    discover(&plugin_root.join(LAYOUT_DIR))
        .into_iter()
        .map(|entry| LayoutEntry {
            id: format!("{plugin}/{}", entry.id),
            ..entry
        })
        .collect()
}

/// 插件带的布局属于哪个插件；用户布局是 None。
pub(crate) fn layout_plugin(id: &str) -> Option<&str> {
    id.split_once('/').map(|(plugin, _)| plugin)
}

fn read_layout(path: &Path) -> Result<LayoutFile, String> {
    let text = std::fs::read_to_string(path).map_err(|error| format!("读不了：{error}"))?;
    LayoutFile::parse(&text)
}

/// 副本的文件名（不带 `.json`）：`<id>-copy`，插件布局 id 里的 `/` 换成 `-`；
/// 已经有了就往后加序号。`taken` 判断某个名字是不是已经有文件。
pub(crate) fn copy_id(from: &str, taken: impl Fn(&str) -> bool) -> String {
    let base = format!("{}-copy", from.replace('/', "-"));
    std::iter::once(base.clone())
        .chain((2..).map(|n| format!("{base}-{n}")))
        .find(|id| !taken(id))
        .expect("序号用不完")
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_component::dock::{PanelInfo, PanelState};

    fn names(state: &PanelState, out: &mut Vec<String>) {
        if !state.children.is_empty() {
            for child in &state.children {
                names(child, out);
            }
        } else {
            out.push(state.panel_name.clone());
        }
    }

    #[test]
    fn the_default_layout_parses_and_places_every_builtin() {
        let file = LayoutFile::default_layout();
        assert_eq!(file.state.version, Some(VERSION));
        assert!(file.state.left_dock.is_none() && file.state.right_dock.is_none());
        let mut placed = Vec::new();
        names(&file.state.center, &mut placed);
        assert_eq!(
            placed,
            [
                panel::SESSIONS,
                panel::STAGE,
                panel::FILES,
                panel::GIT,
                panel::HISTORY
            ]
        );
        assert!(matches!(
            file.state.center.info,
            PanelInfo::Stack { axis: 0, .. }
        ));
    }

    #[test]
    fn a_layout_needs_our_version() {
        let body = r#""center": {"panel_name": "TabPanel", "children": [], "info": {"tabs": {"active_index": 0}}}"#;
        assert!(LayoutFile::parse(&format!("{{\"version\": 1, {body}}}")).is_ok());
        let missing = LayoutFile::parse(&format!("{{{body}}}")).unwrap_err();
        assert!(missing.contains("没写 `version`"), "{missing}");
        let newer = LayoutFile::parse(&format!("{{\"version\": 2, {body}}}")).unwrap_err();
        assert!(newer.contains("`version` 是 2"), "{newer}");
        let broken = LayoutFile::parse("{\"version\": 1}").unwrap_err();
        assert!(broken.contains("不是合法的布局"), "{broken}");
    }

    #[test]
    fn a_saved_state_is_used_only_with_the_file_it_was_saved_for() {
        let file = LayoutFile::default_layout();
        let record = |digest: &str, state: String| LayoutStateRecord {
            layout_id: DEFAULT_ID.into(),
            file_digest: digest.into(),
            state,
        };
        let saved = to_text(&file.state);
        assert_eq!(
            saved_state(&record(&file.digest, saved.clone()), &file),
            Some(file.state.clone())
        );
        assert_eq!(saved_state(&record("changed", saved), &file), None);
        let mut old = file.state.clone();
        old.version = Some(VERSION + 1);
        assert_eq!(saved_state(&record(&file.digest, to_text(&old)), &file), None);
    }

    #[test]
    fn discovery_lists_bad_files_with_a_reason_and_reserves_default() {
        let dir = std::env::temp_dir().join(format!("smelt-layouts-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("b.json"), DEFAULT_LAYOUT).unwrap();
        std::fs::write(dir.join("a.json"), "{").unwrap();
        std::fs::write(dir.join("default.json"), DEFAULT_LAYOUT).unwrap();
        std::fs::write(dir.join("notes.txt"), "").unwrap();

        let entries = discover(&dir);
        let ids: Vec<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "default"]);
        assert!(entries[0].layout.is_err());
        assert!(entries[1].layout.is_ok());
        assert!(entries[2].layout.as_ref().unwrap_err().contains("留给内置"));

        let root = dir.join("plugin");
        std::fs::create_dir_all(root.join(LAYOUT_DIR)).unwrap();
        std::fs::write(root.join(LAYOUT_DIR).join("wide.json"), DEFAULT_LAYOUT).unwrap();
        let entries = discover_plugin("demo", &root);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "demo/wide");
        assert_eq!(layout_plugin(&entries[0].id), Some("demo"));
        assert_eq!(layout_plugin("b"), None);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn copies_get_a_free_name() {
        assert_eq!(copy_id("work", |_| false), "work-copy");
        assert_eq!(copy_id("demo/wide", |_| false), "demo-wide-copy");
        assert_eq!(
            copy_id("work", |id| id == "work-copy" || id == "work-copy-2"),
            "work-copy-3"
        );
    }
}
