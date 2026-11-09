//! 插件带的主题：插件目录下 `themes/*.json`，gpui-kit 标准主题文件（一个文件一个主题系列，
//! `themes` 里可以有明暗好几款）。
//!
//! 主题是数据，不跑脚本，插件脚本加载失败也照样认。名字和内置主题重名的不认（插件不能
//! 顶替内置主题），插件之间重名按加载顺序先到先得；没认的都记到那个插件下面，设置页列出来。

use std::path::Path;
use std::rc::Rc;

use gpui::SharedString;
use gpui_component::{ThemeConfig, ThemeSet};
use smelt_ui::theme_catalog::PluginTheme;

/// 插件目录下放主题文件的子目录。
const THEME_DIR: &str = "themes";

/// 读一个插件的主题文件，按文件名排序。返回读到的主题和读不了的原因。读盘，只在事件里调。
pub(super) fn read(root: &Path) -> (Vec<ThemeConfig>, Vec<String>) {
    let directory = root.join(THEME_DIR);
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return (Vec::new(), Vec::new());
    };
    let mut files: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    files.sort();
    let mut themes = Vec::new();
    let mut problems = Vec::new();
    for file in files {
        let shown = format!(
            "{THEME_DIR}/{}",
            file.file_name().unwrap_or_default().to_string_lossy()
        );
        match std::fs::read_to_string(&file)
            .map_err(|error| error.to_string())
            .and_then(|text| parse(&text))
        {
            Ok(set) => themes.extend(set),
            Err(error) => problems.push(format!("主题文件 {shown} 读不了：{error}")),
        }
    }
    (themes, problems)
}

fn parse(text: &str) -> Result<Vec<ThemeConfig>, String> {
    let set: ThemeSet = serde_json::from_str(text).map_err(|error| error.to_string())?;
    if set.themes.is_empty() {
        return Err("`themes` 里一款主题也没有".into());
    }
    if set.themes.iter().any(|theme| theme.name.trim().is_empty()) {
        return Err("有一款主题没写 name".into());
    }
    Ok(set.themes)
}

/// 一个插件读到的主题，等着和别的插件、内置主题排重。
pub(super) struct Offered {
    pub(super) plugin: String,
    pub(super) plugin_name: String,
    pub(super) themes: Vec<ThemeConfig>,
}

/// 排重：内置的名字不让用，插件之间先到先得。返回认下的主题和没认的 (插件 id, 原因)。
pub(super) fn resolve(
    offered: Vec<Offered>,
    is_built_in: impl Fn(&str) -> bool,
) -> (Vec<PluginTheme>, Vec<(String, String)>) {
    let mut accepted: Vec<(String, PluginTheme)> = Vec::new();
    let mut problems = Vec::new();
    for Offered {
        plugin,
        plugin_name,
        themes,
    } in offered
    {
        for theme in themes {
            let name = theme.name.clone();
            if is_built_in(&name) {
                problems.push((
                    plugin.clone(),
                    format!("主题「{name}」和内置主题重名，没装上：换个名字"),
                ));
                continue;
            }
            if let Some((owner, _)) = accepted
                .iter()
                .find(|(_, accepted)| accepted.config.name == name)
            {
                let problem = if *owner == plugin {
                    format!("主题「{name}」写了两次，用的是先读到的那份")
                } else {
                    format!("主题「{name}」和插件 {owner} 的重名，没装上")
                };
                problems.push((plugin.clone(), problem));
                continue;
            }
            accepted.push((
                plugin.clone(),
                PluginTheme {
                    config: Rc::new(theme),
                    family: SharedString::from(plugin_name.clone()),
                },
            ));
        }
    }
    (
        accepted.into_iter().map(|(_, theme)| theme).collect(),
        problems,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme(name: &str) -> ThemeConfig {
        serde_json::from_str(&format!(r#"{{"name": "{name}", "mode": "dark"}}"#)).unwrap()
    }

    /// 每个测试一个空目录，测完删掉。
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("smelt-plugin-themes-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn offered(plugin: &str, names: &[&str]) -> Offered {
        Offered {
            plugin: plugin.into(),
            plugin_name: format!("{plugin} 的名字"),
            themes: names.iter().map(|name| theme(name)).collect(),
        }
    }

    #[test]
    fn theme_files_are_read_in_name_order_and_bad_ones_reported() {
        let root = Scratch::new("read");
        let themes = root.0.join(THEME_DIR);
        std::fs::create_dir_all(&themes).unwrap();
        std::fs::write(
            themes.join("b.json"),
            r#"{"name": "B", "themes": [{"name": "B Dark", "mode": "dark"}, {"name": "B Light", "mode": "light"}]}"#,
        )
        .unwrap();
        std::fs::write(
            themes.join("a.json"),
            r#"{"name": "A", "themes": [{"name": "A", "mode": "dark"}]}"#,
        )
        .unwrap();
        std::fs::write(themes.join("broken.json"), "{").unwrap();
        std::fs::write(themes.join("empty.json"), r#"{"name": "E", "themes": []}"#).unwrap();
        std::fs::write(themes.join("notes.txt"), "不是主题").unwrap();

        let (read, problems) = read(&root.0);
        let names: Vec<_> = read.iter().map(|theme| theme.name.to_string()).collect();
        assert_eq!(names, ["A", "B Dark", "B Light"]);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems[0].contains("themes/broken.json"));
        assert!(
            problems[1].contains("themes/empty.json") && problems[1].contains("一款主题也没有")
        );
    }

    #[test]
    fn no_theme_directory_means_no_themes() {
        let root = Scratch::new("none");
        let (themes, problems) = read(&root.0);
        assert!(themes.is_empty() && problems.is_empty());
    }

    #[test]
    fn built_in_names_are_refused_and_earlier_plugins_win() {
        let (accepted, problems) = resolve(
            vec![
                offered("first", &["Traveler", "Ayu Dark", "Traveler"]),
                offered("second", &["Traveler", "Night"]),
            ],
            |name| name == "Ayu Dark",
        );
        let names: Vec<_> = accepted
            .iter()
            .map(|theme| (theme.config.name.to_string(), theme.family.to_string()))
            .collect();
        assert_eq!(
            names,
            [
                ("Traveler".to_string(), "first 的名字".to_string()),
                ("Night".to_string(), "second 的名字".to_string()),
            ]
        );
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert_eq!(problems[0].0, "first");
        assert!(problems[0].1.contains("内置主题重名"));
        assert!(problems[1].1.contains("写了两次"));
        assert_eq!(problems[2].0, "second");
        assert!(problems[2].1.contains("插件 first"));
    }
}
