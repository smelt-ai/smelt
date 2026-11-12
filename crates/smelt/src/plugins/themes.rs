//! 插件携带的 gpui-kit 主题文件。只在插件发现、导入或重新加载时读盘。

use std::path::{Path, PathBuf};

/// 读取插件的 `themes/*.json`；纯主题插件可以把 JSON 文件写在 manifest 的 entry。
/// 返回完整结果或错误，避免一个插件的一半主题装上、另一半悄悄丢掉。格式由注册表校验。
pub(super) fn read(root: &Path, entry: &str) -> Result<Vec<String>, String> {
    let directory = root.join("themes");
    let mut files = match std::fs::read_dir(&directory) {
        Ok(entries) => entries
            .map(|entry| {
                entry
                    .map(|entry| entry.path())
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(format!("读取主题目录失败：{error}")),
    };
    files.retain(|path| path.extension().is_some_and(|ext| ext == "json"));
    let entry = Path::new(entry);
    if entry.components().any(|component| {
        !matches!(
            component,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        )
    }) {
        return Err("插件入口必须是插件目录内的相对路径".into());
    }
    if entry.extension().is_some_and(|ext| ext == "json") {
        let entry = root.join(entry);
        if !files.contains(&entry) {
            files.push(entry);
        }
    }
    files.sort();
    files
        .into_iter()
        .map(|path| read_file(root, path))
        .collect()
}

fn read_file(root: &Path, path: PathBuf) -> Result<String, String> {
    let label = path.strip_prefix(root).unwrap_or(&path).display();
    std::fs::read_to_string(&path).map_err(|error| format!("主题文件 {label} 读取失败：{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("smelt-plugin-themes-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn reads_theme_sets_in_filename_order_and_json_entry_once() {
        let dir = Scratch::new();
        let themes = dir.path().join("themes");
        std::fs::create_dir(&themes).unwrap();
        std::fs::write(
            themes.join("b.json"),
            r#"{"name":"B","themes":[{"name":"B Light","mode":"light"}]}"#,
        )
        .unwrap();
        std::fs::write(
            themes.join("a.json"),
            r#"{"name":"A","themes":[{"name":"A Dark","mode":"dark"}]}"#,
        )
        .unwrap();
        let sets = read(dir.path(), "themes/a.json").unwrap();
        let names = sets
            .iter()
            .map(|json| {
                serde_json::from_str::<gpui_component::ThemeSet>(json)
                    .unwrap()
                    .name
            })
            .collect::<Vec<_>>();
        assert_eq!(names, ["A", "B"]);
    }

    #[test]
    fn invalid_theme_file_is_rejected_atomically_by_catalog() {
        let dir = Scratch::new();
        let themes = dir.path().join("themes");
        std::fs::create_dir(&themes).unwrap();
        std::fs::write(
            themes.join("a.json"),
            r#"{"name":"A","themes":[{"name":"A Dark","mode":"dark"}]}"#,
        )
        .unwrap();
        std::fs::write(themes.join("broken.json"), "{").unwrap();
        let sets = read(dir.path(), "main.js").unwrap();
        let sources = sets.iter().map(String::as_str).collect::<Vec<_>>();
        let mut catalog = smelt_ui::theme_catalog::ThemeCatalog::default();
        assert!(catalog.register("plugin", &sources, |_| false).is_err());
        assert!(catalog.themes().next().is_none());
    }

    #[test]
    fn entry_cannot_escape_plugin_directory() {
        let dir = Scratch::new();
        assert!(read(dir.path(), "./main.js").is_ok());
        assert!(read(dir.path(), "../outside.json").is_err());
        assert!(read(dir.path(), "/tmp/outside.json").is_err());
    }

    #[test]
    fn missing_theme_directory_is_not_an_error_for_script_plugins() {
        let dir = Scratch::new();
        assert!(read(dir.path(), "main.js").unwrap().is_empty());
    }
}
