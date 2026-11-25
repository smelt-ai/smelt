//! `smelt plugin check <目录>`：离线校验插件清单、locales、主题文件、TSX 打包、import 与
//! QuickJS 编译，以及主题名有没有和已装的插件撞；不修改插件目录，过了输出 JSON 并退出 0，
//! 没过把原因写 stderr 并退出 1。

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use rust_i18n::t;

/// 检查 `dir` 里的插件，返回退出码。
pub(super) fn run(dir: &Path) -> i32 {
    let root = match std::fs::canonicalize(dir) {
        Ok(root) => root,
        Err(error) => {
            return fail(&t!(
                "plugins.cli_no_dir",
                path = dir.display(),
                error = error
            ));
        }
    };
    if !root.join(smelt_nsh::MANIFEST_FILE).is_file() {
        return fail(&t!("plugins.no_manifest", file = smelt_nsh::MANIFEST_FILE));
    }
    let mut sources = super::module_sources();
    if let Some(parent) = root.parent().filter(|p| !p.as_os_str().is_empty())
        && !sources.iter().any(|s| s.path == parent)
    {
        sources.push(smelt_nsh::SourceDir {
            path: parent.to_path_buf(),
            source: smelt_nsh::PluginSource::Builtin,
            watch: false,
        });
    }
    match smelt_nsh::check_plugin_with_sources(&root, &sources) {
        Ok(report) => {
            let installed = super::scan_theme_manifests(super::builtin::dir())
                .into_iter()
                // 用户目录里同 id 的顶掉自带的，和运行时一样。
                .chain(super::scan_theme_manifests(
                    super::user_plugin_dir().as_deref(),
                ))
                .collect();
            if let Err(error) = check_theme_names(
                &report.manifest.id,
                &root,
                &report.manifest.entry,
                installed,
            ) {
                return fail(&error);
            }
            let mut out = serde_json::json!({
                "ok": true,
                "id": report.manifest.id,
                "name": report.manifest.name,
                "version": report.manifest.version,
                "path": root,
                "theme_only": report.theme_only,
                "layout_only": report.layout_only,
                "themes": report.themes,
                "exports": report.exports,
            });
            if let Some(obj) = out.as_object_mut() {
                if let Some(module) = report.manifest.module {
                    obj.insert("module".to_owned(), serde_json::Value::String(module));
                }
                if !report.manifest.requires.is_empty() {
                    obj.insert(
                        "requires".to_owned(),
                        serde_json::json!(report.manifest.requires),
                    );
                }
            }
            println!("{out:#}");
            0
        }
        Err(error) => fail(&error),
    }
}

/// 主题名不能和别的插件撞：撞了运行时这个插件的主题整组不生效，单看插件自己查不出来。
/// 照运行时的样子先登记已装的插件（`id` 自己那份除外：它会被这一份换掉），再登记这一个。
fn check_theme_names(
    id: &str,
    root: &Path,
    entry: &str,
    installed: BTreeMap<String, (PathBuf, smelt_nsh::PluginManifest)>,
) -> Result<(), String> {
    let mine = super::themes::read(root, entry)?;
    if mine.is_empty() {
        return Ok(());
    }
    let mut catalog = smelt_ui::theme_catalog::ThemeCatalog::default();
    for (other, (other_root, manifest)) in installed {
        if other == id || other_root == root {
            continue;
        }
        // 别的插件自己坏了的，运行时也登记不上，不算撞。
        if let Ok(sources) = super::themes::read(&other_root, &manifest.entry) {
            let refs: Vec<&str> = sources.iter().map(String::as_str).collect();
            let _ = catalog.register(
                &format!("files:{other}"),
                &refs,
                smelt_ui::theme_catalog::is_reserved,
            );
        }
    }
    let refs: Vec<&str> = mine.iter().map(String::as_str).collect();
    catalog
        .register(
            &format!("files:{id}"),
            &refs,
            smelt_ui::theme_catalog::is_reserved,
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn fail(message: &str) -> i32 {
    eprintln!("{message}");
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme_plugin(dir: &Path, id: &str, theme: &str) -> PathBuf {
        let root = dir.join(id);
        std::fs::create_dir_all(root.join("themes")).unwrap();
        std::fs::write(
            root.join(smelt_nsh::MANIFEST_FILE),
            format!(
                r#"{{"id":"{id}","name":"{id}","version":"0.1.0","entry":"themes/theme.json"}}"#
            ),
        )
        .unwrap();
        std::fs::write(
            root.join("themes/theme.json"),
            format!(
                r##"{{"version":1,"name":"{theme}","themes":[{{"name":"{theme} Dark","mode":"dark","colors":{{"bg_base":"#101010","text_base":"#fafafa"}}}}]}}"##
            ),
        )
        .unwrap();
        root
    }

    #[test]
    fn a_theme_name_taken_by_another_plugin_fails_but_its_own_old_version_does_not() {
        let dir = std::env::temp_dir().join(format!("smelt-check-themes-{}", uuid::Uuid::new_v4()));
        let installed_root = theme_plugin(&dir.join("installed"), "smelt.themes", "Ayu");
        let installed = || {
            let manifest = smelt_nsh::PluginManifest::read(&installed_root).unwrap();
            BTreeMap::from([(
                "smelt.themes".to_owned(),
                (installed_root.clone(), manifest),
            )])
        };

        // 别的 id 用了同一个主题名：撞。
        let copy = theme_plugin(&dir.join("mine"), "me.themes", "Ayu");
        let error =
            check_theme_names("me.themes", &copy, "themes/theme.json", installed()).unwrap_err();
        assert!(error.contains("Ayu Dark"), "{error}");

        // 同 id 覆写：换掉的是自己，不算撞。
        let fork = theme_plugin(&dir.join("fork"), "smelt.themes", "Ayu");
        check_theme_names("smelt.themes", &fork, "themes/theme.json", installed()).unwrap();

        // 换了名字就过。
        let renamed = theme_plugin(&dir.join("renamed"), "me.sunset", "Sunset");
        check_theme_names("me.sunset", &renamed, "themes/theme.json", installed()).unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }
}
