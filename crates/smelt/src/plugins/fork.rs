//! `smelt plugin fork <builtin-id> [--as <new-id>] [--dir <目录>]`：把自带插件源码复制到
//! 用户插件目录，用于同 id 原地覆写或 `--as` 改名并存。

use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use rust_i18n::t;

/// 从命令行参数复制自带插件源码到用户目录，返回退出码（0 成功，1 失败，2 用法错）。
pub(super) fn run(args: &[String]) -> i32 {
    let parsed = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}");
            eprintln!("{}", t!("plugins.cli_fork_usage"));
            return 2;
        }
    };
    let Some(builtin_dir) = super::builtin::dir() else {
        return fail(&t!("plugins.cli_fork_not_found", id = parsed.builtin_id));
    };
    let Some(builtin_root) = super::manage::installed_root(builtin_dir, &parsed.builtin_id) else {
        return fail(&t!("plugins.cli_fork_not_found", id = parsed.builtin_id));
    };

    let target_id = parsed.as_id.unwrap_or_else(|| parsed.builtin_id.clone());
    if !smelt_nsh::is_valid_plugin_id(&target_id) {
        return fail(&t!("plugins.cli_bad_id", id = target_id));
    }
    if target_id != parsed.builtin_id
        && super::manage::installed_root(builtin_dir, &target_id).is_some()
    {
        return fail(&t!("plugins.builtin_id", id = target_id));
    }

    let target_dir = match parsed.dir {
        Some(dir) => dir,
        None => match super::manage::user_plugin_dir() {
            Some(dir) => dir.join(&target_id),
            None => return fail(&t!("plugins.no_data_dir")),
        },
    };

    match fork_into(&builtin_root, &target_dir, &parsed.builtin_id, &target_id) {
        Ok(report) => {
            println!("{report:#}");
            0
        }
        Err(error) => fail(&error),
    }
}

struct ForkArgs {
    builtin_id: String,
    as_id: Option<String>,
    dir: Option<PathBuf>,
}

fn parse_args(args: &[String]) -> Result<ForkArgs, String> {
    let mut builtin_id: Option<String> = None;
    let mut as_id: Option<String> = None;
    let mut dir: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--as" => {
                let value = args
                    .get(i + 1)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| t!("plugins.cli_fork_usage").into_owned())?;
                as_id = Some(value.clone());
                i += 2;
            }
            "--dir" => {
                let value = args
                    .get(i + 1)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| t!("plugins.cli_fork_usage").into_owned())?;
                dir = Some(PathBuf::from(value));
                i += 2;
            }
            flag if flag.starts_with('-') => {
                return Err(t!("plugins.cli_fork_usage").into_owned());
            }
            value => {
                if builtin_id.is_some() {
                    return Err(t!("plugins.cli_fork_usage").into_owned());
                }
                builtin_id = Some(value.to_owned());
                i += 1;
            }
        }
    }
    let builtin_id = builtin_id.ok_or_else(|| t!("plugins.cli_fork_usage").into_owned())?;
    Ok(ForkArgs {
        builtin_id,
        as_id,
        dir,
    })
}

fn normalize_relative(entry: &str) -> Result<PathBuf, String> {
    let path = Path::new(entry);
    if path.is_absolute()
        || path.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(format!("插件入口必须是插件目录内的相对路径：{entry}"));
    }
    Ok(path
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect())
}

pub(super) fn fork_into(
    builtin_root: &Path,
    target_dir: &Path,
    builtin_id: &str,
    target_id: &str,
) -> Result<serde_json::Value, String> {
    let manifest = smelt_nsh::PluginManifest::read(builtin_root)?;
    let normalized_entry = normalize_relative(&manifest.entry)?;
    if !builtin_root.join(&normalized_entry).is_file() {
        return Err(t!(
            "plugins.cli_fork_no_source",
            id = builtin_id,
            entry = manifest.entry
        )
        .into_owned());
    }
    if target_dir.join(smelt_nsh::MANIFEST_FILE).exists() {
        return Err(t!("plugins.cli_fork_exists", path = target_dir.display()).into_owned());
    }

    let existed = fs::symlink_metadata(target_dir).is_ok();
    let copy_result = (|| -> Result<(), String> {
        fs::create_dir_all(target_dir)
            .map_err(|error| t!("plugins.create_dir_failed", error = error).into_owned())?;
        super::manage::copy_plugin_source(builtin_root, target_dir)
            .map_err(|error| t!("plugins.copy_failed", error = error).into_owned())
    })();
    if let Err(error) = copy_result {
        if !existed {
            let _ = fs::remove_dir_all(target_dir);
        }
        return Err(error);
    }

    let root = fs::canonicalize(target_dir).unwrap_or_else(|_| target_dir.to_path_buf());
    if target_id != builtin_id {
        let manifest_path = root.join(smelt_nsh::MANIFEST_FILE);
        let raw = fs::read_to_string(&manifest_path)
            .map_err(|error| format!("读取 {} 失败：{error}", manifest_path.display()))?;
        let mut json: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|error| format!("解析 {} 失败：{error}", manifest_path.display()))?;
        if let Some(obj) = json.as_object_mut() {
            obj.insert(
                "id".to_owned(),
                serde_json::Value::String(target_id.to_owned()),
            );
        }
        let updated = format!(
            "{}\n",
            serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?
        );
        fs::write(&manifest_path, updated)
            .map_err(|error| format!("写入 {} 失败：{error}", manifest_path.display()))?;
    }

    let theme_only = manifest.is_theme_only();
    let mut types_generated = false;
    if !theme_only {
        let gitignore = root.join(".gitignore");
        if !gitignore.exists() {
            fs::write(&gitignore, ".nsh/\nnode_modules/\n")
                .map_err(|error| format!("写入 .gitignore 失败：{error}"))?;
        }
        smelt_nsh::write_plugin_types(&root)?;
        types_generated = true;
    }

    let source_path = fs::canonicalize(builtin_root).unwrap_or_else(|_| builtin_root.to_path_buf());
    Ok(serde_json::json!({
        "ok": true,
        "id": target_id,
        "from_builtin": builtin_id,
        "overrides_builtin": target_id == builtin_id,
        "theme_only": theme_only,
        "path": root,
        "source_path": source_path,
        "types_generated": types_generated,
    }))
}

fn fail(message: &str) -> i32 {
    eprintln!("{message}");
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "smelt-plugin-fork-{label}-{}",
                uuid::Uuid::new_v4()
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn fork_same_id_overrides_builtin_strips_bytecode_and_passes_check() {
        let scratch = Scratch::new("override");
        let builtin = scratch.0.join("staged/files");
        fs::create_dir_all(builtin.join("tests")).unwrap();
        fs::write(
            builtin.join(smelt_nsh::MANIFEST_FILE),
            r#"{"id":"smelt.example","name":"Example","version":"1.0.0","entry":"nsh.tsx"}"#,
        )
        .unwrap();
        fs::write(
            builtin.join("nsh.tsx"),
            "import { Text, View } from \"nsh\";\nexport function Main() { return <View><Text>ok</Text></View>; }\n",
        )
        .unwrap();
        fs::write(builtin.join("tests/unit.test.mjs"), "test()").unwrap();
        fs::write(builtin.join("bundle.nshc"), b"bytecode").unwrap();
        fs::write(builtin.join("bundle.meta.json"), "{}").unwrap();

        let target = scratch.0.join("user/smelt.example");
        let report = fork_into(&builtin, &target, "smelt.example", "smelt.example").unwrap();
        assert_eq!(report["ok"], true);
        assert_eq!(report["id"], "smelt.example");
        assert_eq!(report["from_builtin"], "smelt.example");
        assert_eq!(report["overrides_builtin"], true);
        assert_eq!(report["theme_only"], false);
        assert_eq!(report["types_generated"], true);
        assert!(!target.join("bundle.nshc").exists());
        assert!(!target.join("bundle.meta.json").exists());
        assert!(!target.join("tests").exists());
        assert!(target.join(".nsh/types/smelt/sdk.d.ts").is_file());
        assert!(target.join("tsconfig.json").is_file());

        let checked = smelt_nsh::check_plugin(&target).unwrap();
        assert_eq!(checked.manifest.id, "smelt.example");
        assert_eq!(checked.exports, vec!["Main"]);
    }

    #[test]
    fn fork_with_as_rewrites_id_and_theme_only_skips_types() {
        let scratch = Scratch::new("theme");
        let builtin = scratch.0.join("builtin/themes");
        fs::create_dir_all(builtin.join("themes")).unwrap();
        fs::write(
            builtin.join(smelt_nsh::MANIFEST_FILE),
            r#"{"id":"smelt.themes","name":"Themes","version":"1.0.0","entry":"themes/t.json"}"#,
        )
        .unwrap();
        fs::write(
            builtin.join("themes/t.json"),
            r##"{"version":1,"name":"Pack","themes":[{"name":"Pack Dark","mode":"dark","colors":{"bg_base":"#111111","text_base":"#eeeeee"}}]}"##,
        )
        .unwrap();

        let target = scratch.0.join("user/me.themes");
        let report = fork_into(&builtin, &target, "smelt.themes", "me.themes").unwrap();
        assert_eq!(report["id"], "me.themes");
        assert_eq!(report["overrides_builtin"], false);
        assert_eq!(report["theme_only"], true);
        assert_eq!(report["types_generated"], false);
        assert!(!target.join(".nsh").exists());
        assert!(!target.join("tsconfig.json").exists());

        let manifest = smelt_nsh::PluginManifest::read(&target).unwrap();
        assert_eq!(manifest.id, "me.themes");
        let checked = smelt_nsh::check_plugin(&target).unwrap();
        assert!(checked.theme_only);
        assert_eq!(checked.themes, vec!["Pack Dark"]);
    }

    #[test]
    fn fork_refuses_missing_builtin_missing_source_or_existing_target() {
        let scratch = Scratch::new("errors");
        let builtin = scratch.0.join("staged/broken");
        fs::create_dir_all(&builtin).unwrap();
        fs::write(
            builtin.join(smelt_nsh::MANIFEST_FILE),
            r#"{"id":"smelt.broken","name":"Broken","version":"1.0.0","entry":"nsh.tsx"}"#,
        )
        .unwrap();
        fs::write(builtin.join("bundle.nshc"), b"bytecode").unwrap();

        let target = scratch.0.join("user/smelt.broken");
        assert!(fork_into(&builtin, &target, "smelt.broken", "smelt.broken").is_err());
        assert!(!target.exists());

        fs::write(
            builtin.join("nsh.tsx"),
            "export function Main() { return null; }\n",
        )
        .unwrap();
        fork_into(&builtin, &target, "smelt.broken", "smelt.broken").unwrap();
        assert!(fork_into(&builtin, &target, "smelt.broken", "smelt.broken").is_err());

        assert_eq!(run(&["smelt.does-not-exist".into()]), 1);
        assert_eq!(run(&[]), 2);
        assert_eq!(run(&["smelt.files".into(), "--unknown".into()]), 2);
    }
}
