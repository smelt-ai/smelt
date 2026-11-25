//! `smelt plugin init <id> [--theme] [--name <名称>] [--dir <目录>]`：创建带 TypeScript 声明的起步插件，或加 `--theme` 创建纯主题插件。

use std::{
    fs,
    path::{Path, PathBuf},
};

use rust_i18n::t;

/// 从命令行参数脚手架创建 NSH TSX 插件，返回退出码（0 成功，1 失败，2 用法错）。
pub(super) fn run(args: &[String]) -> i32 {
    let parsed = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}");
            eprintln!("{}", t!("plugins.cli_init_usage"));
            return 2;
        }
    };
    if !smelt_nsh::is_valid_plugin_id(&parsed.id) {
        return fail(&t!("plugins.cli_bad_id", id = parsed.id));
    }
    if super::builtin::dir()
        .is_some_and(|dir| super::manage::installed_root(dir, &parsed.id).is_some())
    {
        return fail(&t!("plugins.builtin_id", id = parsed.id));
    }
    let target = match parsed.dir {
        Some(dir) => dir,
        None => match super::manage::user_plugin_dir() {
            Some(dir) => dir.join(&parsed.id),
            None => return fail(&t!("plugins.no_data_dir")),
        },
    };
    let name = parsed
        .name
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| default_display_name(&parsed.id));

    let result = if parsed.theme {
        scaffold_theme(&target, &parsed.id, &name)
    } else {
        scaffold(&target, &parsed.id, &name)
    };
    match result {
        Ok(report) => {
            println!("{report:#}");
            0
        }
        Err(error) => fail(&error),
    }
}

struct InitArgs {
    id: String,
    name: Option<String>,
    dir: Option<PathBuf>,
    theme: bool,
}

fn parse_args(args: &[String]) -> Result<InitArgs, String> {
    let mut id: Option<String> = None;
    let mut name: Option<String> = None;
    let mut dir: Option<PathBuf> = None;
    let mut theme = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--theme" => {
                theme = true;
                i += 1;
            }
            "--name" => {
                let value = args
                    .get(i + 1)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| t!("plugins.cli_init_usage").into_owned())?;
                name = Some(value.clone());
                i += 2;
            }
            "--dir" => {
                let value = args
                    .get(i + 1)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| t!("plugins.cli_init_usage").into_owned())?;
                dir = Some(PathBuf::from(value));
                i += 2;
            }
            flag if flag.starts_with('-') => {
                return Err(t!("plugins.cli_init_usage").into_owned());
            }
            value => {
                if id.is_some() {
                    return Err(t!("plugins.cli_init_usage").into_owned());
                }
                id = Some(value.to_owned());
                i += 1;
            }
        }
    }
    let id = id.ok_or_else(|| t!("plugins.cli_init_usage").into_owned())?;
    Ok(InitArgs {
        id,
        name,
        dir,
        theme,
    })
}

fn default_display_name(id: &str) -> String {
    let tail = id.rsplit('.').next().unwrap_or(id);
    let words: Vec<String> = tail
        .split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect();
    if words.is_empty() {
        id.to_owned()
    } else {
        words.join(" ")
    }
}

pub(super) fn scaffold(dir: &Path, id: &str, name: &str) -> Result<serde_json::Value, String> {
    if dir.join(smelt_nsh::MANIFEST_FILE).exists() || dir.join("nsh.tsx").exists() {
        return Err(t!("plugins.cli_init_exists", path = dir.display()).into_owned());
    }
    fs::create_dir_all(dir)
        .map_err(|error| t!("plugins.create_dir_failed", error = error).into_owned())?;
    let root = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());

    let manifest = serde_json::json!({
        "id": id,
        "name": name,
        "version": "0.1.0",
        "entry": "nsh.tsx",
    });
    let manifest_text = format!(
        "{}\n",
        serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?
    );
    fs::write(root.join(smelt_nsh::MANIFEST_FILE), manifest_text)
        .map_err(|error| format!("写入 {} 失败：{error}", smelt_nsh::MANIFEST_FILE))?;

    fs::write(root.join("nsh.tsx"), STARTER_ENTRY)
        .map_err(|error| format!("写入 nsh.tsx 失败：{error}"))?;
    // 文案一开始就走 locales：起步插件是照着改的样板，样板写死一种语言，改出来的也是。
    let escaped_name = serde_json::to_string(name).map_err(|e| e.to_string())?;
    fs::create_dir_all(root.join("locales"))
        .map_err(|error| t!("plugins.create_dir_failed", error = error).into_owned())?;
    fs::write(
        root.join("locales/ui.yml"),
        STARTER_LOCALES.replace("__NAME_JSON__", &escaped_name),
    )
    .map_err(|error| format!("写入 locales/ui.yml 失败：{error}"))?;

    let gitignore = root.join(".gitignore");
    let created_gitignore = if !gitignore.exists() {
        fs::write(&gitignore, ".nsh/\nnode_modules/\n")
            .map_err(|error| format!("写入 .gitignore 失败：{error}"))?;
        true
    } else {
        false
    };

    let sources = super::module_sources();
    let types = smelt_nsh::write_plugin_types(&root, &sources)?;

    let mut files = vec![
        smelt_nsh::MANIFEST_FILE.to_owned(),
        "nsh.tsx".to_owned(),
        "locales/ui.yml".to_owned(),
        ".nsh/tsconfig.json".to_owned(),
        ".nsh/types/".to_owned(),
    ];
    if created_gitignore {
        files.push(".gitignore".to_owned());
    }
    if types.created_tsconfig {
        files.push("tsconfig.json".to_owned());
    }
    files.sort();

    Ok(serde_json::json!({
        "ok": true,
        "id": id,
        "name": name,
        "path": root,
        "view": format!("{id}/main"),
        "files": files,
    }))
}

pub(super) fn scaffold_theme(
    dir: &Path,
    id: &str,
    name: &str,
) -> Result<serde_json::Value, String> {
    if dir.join(smelt_nsh::MANIFEST_FILE).exists() || dir.join("themes/theme.json").exists() {
        return Err(t!("plugins.cli_init_exists", path = dir.display()).into_owned());
    }

    let light_name = format!("{name} Light");
    let dark_name = format!("{name} Dark");
    let theme_json = serde_json::json!({
        "version": 1,
        "name": name,
        "themes": [
            {
                "name": light_name,
                "mode": "light",
                "colors": {
                    "bg_base": "#ffffff",
                    "text_base": "#1a1a18",
                    "bg_accent": "#3a6ee1"
                }
            },
            {
                "name": dark_name,
                "mode": "dark",
                "colors": {
                    "bg_base": "#16171a",
                    "text_base": "#e6e7eb",
                    "bg_accent": "#5b8def"
                }
            }
        ]
    });
    let theme_text = format!(
        "{}\n",
        serde_json::to_string_pretty(&theme_json).map_err(|e| e.to_string())?
    );
    let mut catalog = smelt_ui::theme_catalog::ThemeCatalog::default();
    let themes: Vec<String> = catalog
        .register(
            &format!("files:{id}"),
            &[theme_text.as_str()],
            smelt_ui::theme_catalog::is_reserved,
        )
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|name| name.to_string())
        .collect();

    let themes_dir = dir.join("themes");
    fs::create_dir_all(&themes_dir)
        .map_err(|error| t!("plugins.create_dir_failed", error = error).into_owned())?;
    let root = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());

    let manifest = serde_json::json!({
        "id": id,
        "name": name,
        "version": "0.1.0",
        "entry": "themes/theme.json",
    });
    let manifest_text = format!(
        "{}\n",
        serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?
    );
    fs::write(root.join(smelt_nsh::MANIFEST_FILE), manifest_text)
        .map_err(|error| format!("写入 {} 失败：{error}", smelt_nsh::MANIFEST_FILE))?;
    fs::write(root.join("themes/theme.json"), theme_text)
        .map_err(|error| format!("写入 themes/theme.json 失败：{error}"))?;

    Ok(serde_json::json!({
        "ok": true,
        "id": id,
        "name": name,
        "path": root,
        "theme_only": true,
        "themes": themes,
        "files": [smelt_nsh::MANIFEST_FILE, "themes/theme.json"],
    }))
}

const STARTER_ENTRY: &str = r#"import { signal, Text, View } from "nsh";
import { bootTask, initHost, registerViews, SmeltButton, SmeltIcon, t, text } from "smelt/sdk";

const count = signal(0);

bootTask(async () => {
  await initHost();
  await registerViews([
    {
      name: "main",
      export: "Main",
      title: text("title"),
      icon: "icons/puzzle.svg",
    },
  ]);
});

export function Main() {
  return (
    <View class="flex-1 items-center justify-center gap-3 bg-background p-6">
      <View class="flex-row items-center gap-2">
        <SmeltIcon path="icons/puzzle.svg" size={16} />
        <Text class="text-sm font-semibold text-foreground">{t("title")}</Text>
      </View>
      <Text class="text-xs text-muted-foreground">{t("count", { n: count() })}</Text>
      <SmeltButton label={t("add")} primary onPress={() => count.update((n) => n + 1)} />
    </View>
  );
}
"#;

const STARTER_LOCALES: &str = r#"_version: 2
title:
  en: __NAME_JSON__
  zh-CN: __NAME_JSON__
count:
  en: "Count: %{n}"
  zh-CN: "计数：%{n}"
add:
  en: Add one
  zh-CN: 加一
"#;

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
                "smelt-plugin-init-{label}-{}",
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
    fn scaffold_creates_valid_plugin_that_passes_check() {
        let scratch = Scratch::new("valid");
        let dir = scratch.0.join("com.example.hello");

        let report = scaffold(&dir, "com.example.hello", "Hello World").unwrap();
        assert_eq!(report["ok"], true);
        assert_eq!(report["view"], "com.example.hello/main");
        assert!(dir.join(smelt_nsh::MANIFEST_FILE).is_file());
        assert!(dir.join("nsh.tsx").is_file());
        assert!(dir.join("locales/ui.yml").is_file());
        assert!(dir.join(".gitignore").is_file());
        assert!(dir.join("tsconfig.json").is_file());
        assert!(dir.join(".nsh/tsconfig.json").is_file());
        assert!(dir.join(".nsh/types/smelt/sdk.d.ts").is_file());

        let checked = smelt_nsh::check_plugin(&dir).unwrap();
        assert_eq!(checked.manifest.id, "com.example.hello");
        assert_eq!(checked.manifest.name, "Hello World");
        assert!(!checked.theme_only);
        assert_eq!(checked.exports, vec!["Main"]);
    }

    #[test]
    fn scaffold_handles_quotes_and_braces_in_display_name() {
        let scratch = Scratch::new("escaped");
        let dir = scratch.0.join("com.example.quoted");

        scaffold(&dir, "com.example.quoted", "A \"quoted\" {Plugin} <test>").unwrap();
        let checked = smelt_nsh::check_plugin(&dir).unwrap();
        assert_eq!(checked.exports, vec!["Main"]);
    }

    #[test]
    fn scaffold_refuses_existing_plugin_or_bad_args() {
        let scratch = Scratch::new("dup");
        let dir = scratch.0.join("com.example.hello");
        scaffold(&dir, "com.example.hello", "Hello").unwrap();

        assert!(scaffold(&dir, "com.example.hello", "Hello").is_err());
        assert_eq!(run(&["../bad".into()]), 1);
        assert_eq!(run(&["smelt.git".into()]), 1);
        assert_eq!(run(&[]), 2);
        assert_eq!(run(&["com.example.ok".into(), "--unknown".into()]), 2);
        assert_eq!(run(&["com.example.ok".into(), "--name".into()]), 2);
    }

    #[test]
    fn default_display_name_formats_tail_words() {
        assert_eq!(default_display_name("com.example.my-tool"), "My Tool");
        assert_eq!(
            default_display_name("com.example.hello_world"),
            "Hello World"
        );
        assert_eq!(default_display_name("single"), "Single");
    }

    #[test]
    fn scaffold_theme_creates_valid_theme_plugin_that_passes_check() {
        let scratch = Scratch::new("theme");
        let dir = scratch.0.join("me.sunset");

        let report = scaffold_theme(&dir, "me.sunset", "Sunset").unwrap();
        assert_eq!(report["ok"], true);
        assert_eq!(report["theme_only"], true);
        assert_eq!(
            report["themes"],
            serde_json::json!(["Sunset Light", "Sunset Dark"])
        );
        assert!(dir.join(smelt_nsh::MANIFEST_FILE).is_file());
        assert!(dir.join("themes/theme.json").is_file());
        assert!(!dir.join("nsh.tsx").exists());
        assert!(!dir.join(".nsh").exists());
        assert!(!dir.join("tsconfig.json").exists());

        let checked = smelt_nsh::check_plugin(&dir).unwrap();
        assert!(checked.theme_only);
        assert!(checked.exports.is_empty());
        assert_eq!(checked.themes, vec!["Sunset Light", "Sunset Dark"]);
    }

    #[test]
    fn scaffold_theme_rejects_reserved_name_without_writing_files() {
        let scratch = Scratch::new("reserved");
        let dir = scratch.0.join("missing/me.smelt-copy");

        let err = scaffold_theme(&dir, "me.smelt-copy", "smelt").unwrap_err();
        assert!(err.contains("保留名") || err.contains("smelt"), "{err}");
        assert!(!dir.exists());
    }

    #[test]
    fn parse_args_accepts_theme_flag_anywhere() {
        let parsed = parse_args(&[
            "--theme".into(),
            "me.sunset".into(),
            "--name".into(),
            "Sunset".into(),
            "--dir".into(),
            "/tmp/sunset".into(),
        ])
        .unwrap();
        assert!(parsed.theme);
        assert_eq!(parsed.id, "me.sunset");
        assert_eq!(parsed.name.as_deref(), Some("Sunset"));
        assert_eq!(parsed.dir, Some(PathBuf::from("/tmp/sunset")));

        let after = parse_args(&["me.sunset".into(), "--theme".into()]).unwrap();
        assert!(after.theme);
        assert_eq!(after.id, "me.sunset");
    }
}
