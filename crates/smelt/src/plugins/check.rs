//! `smelt plugin check <目录>`：离线校验插件清单、locales、主题文件、TSX 打包、import 与
//! QuickJS 编译；不修改插件目录，过了输出 JSON 并退出 0，没过把原因写 stderr 并退出 1。

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
            if let Some(module) = report.manifest.module
                && let Some(obj) = out.as_object_mut()
            {
                obj.insert("module".to_owned(), serde_json::Value::String(module));
            }
            println!("{out:#}");
            0
        }
        Err(error) => fail(&error),
    }
}

fn fail(message: &str) -> i32 {
    eprintln!("{message}");
    1
}
