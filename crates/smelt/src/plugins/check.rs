//! `smelt plugin check <目录>`：不开 App，校验插件清单、locales、主题文件以及 TSX 打包与外部
//! import；过了输出 JSON 并退出 0，没过把原因写 stderr 并退出 1。

use std::path::Path;

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
    let manifest = match smelt_nsh::PluginManifest::read(&root) {
        Ok(manifest) => manifest,
        Err(error) => return fail(&t!("plugins.bad_manifest", error = error)),
    };
    if !root.join(&manifest.entry).is_file() {
        return fail(&t!("plugins.no_entry", entry = manifest.entry));
    }
    match validate(&root, &manifest) {
        Ok(()) => {
            let report = serde_json::json!({
                "ok": true,
                "id": manifest.id,
                "name": manifest.name,
                "path": root,
            });
            println!("{report:#}");
            0
        }
        Err(error) => fail(&error),
    }
}

fn validate(root: &Path, manifest: &smelt_nsh::PluginManifest) -> Result<(), String> {
    let themes = super::themes::read(root, &manifest.entry)?;
    if !themes.is_empty() {
        let sources: Vec<&str> = themes.iter().map(String::as_str).collect();
        let mut catalog = smelt_ui::theme_catalog::ThemeCatalog::default();
        catalog
            .register(&format!("files:{}", manifest.id), &sources, |_| false)
            .map_err(|error| error.to_string())?;
    }
    smelt_nsh::check_plugin(root)?;
    Ok(())
}

fn fail(message: &str) -> i32 {
    eprintln!("{message}");
    1
}
