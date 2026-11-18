//! 各 crate 的界面文案（`locales/ui.yml`，见 `docs/i18n.md`）：
//!
//! 1. 每条文案 App 带的每种语言都有；
//! 2. 代码里 `t!("…")`、`text!("…")` 取的键都在本 crate 的文案里；
//! 3. 文案里的键都有代码在取，不留死条目；
//! 4. 用了 `t!` / `text!` 的 crate 都带着文案。
//!
//! 文案用 rust-i18n 构建时同一个加载函数读，和编译进去的一致。只认字面量的键：键写成
//! 变量的取法这里查不到，约定不这么写（按枚举挑字时每个分支写一个字面量）。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use smelt_plugin_api::LOCALES;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `crates/` 下所有 crate 的目录（带 Cargo.toml 的），不进 `target`。
fn crate_dirs(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        if name == "target" || name == "node_modules" || name == "src" {
            continue;
        }
        if path.join("Cargo.toml").is_file() {
            found.push(path.clone());
        }
        crate_dirs(&path, found);
    }
}

fn rust_files(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
}

/// 源码里 `t!("key"` 和 `text!("key"` 的键。前面紧挨着字母、数字、下划线的不算（`format!(`
/// 也以 `t!(` 结尾）。
fn used_keys(source: &str) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for macro_name in ["t!(", "text!("] {
        let mut rest = source;
        let mut offset = 0;
        while let Some(at) = rest.find(macro_name) {
            let start = offset + at;
            let before = source[..start].chars().next_back();
            let after = &source[start + macro_name.len()..];
            rest = &rest[at + macro_name.len()..];
            offset = start + macro_name.len();
            if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                continue;
            }
            let after = after.trim_start();
            let Some(after) = after.strip_prefix('"') else {
                continue;
            };
            if let Some(end) = after.find('"') {
                keys.insert(after[..end].to_string());
            }
        }
    }
    keys
}

#[test]
fn every_catalog_is_complete_and_matches_its_code() {
    let mut crates = Vec::new();
    crate_dirs(&repo_root().join("crates"), &mut crates);
    let mut problems = Vec::new();
    for dir in crates {
        let name = dir
            .strip_prefix(repo_root())
            .unwrap_or(&dir)
            .display()
            .to_string();
        let mut sources = Vec::new();
        rust_files(&dir.join("src"), &mut sources);
        let mut used: BTreeMap<String, PathBuf> = BTreeMap::new();
        for file in &sources {
            // 注释里举例的不算（文档里会写 `text!("…")` 的用法）。
            let source: String = std::fs::read_to_string(file)
                .unwrap_or_default()
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .flat_map(|line| [line, "\n"])
                .collect();
            for key in used_keys(&source) {
                used.entry(key).or_insert_with(|| file.clone());
            }
        }
        let locales_dir = dir.join("locales");
        if !locales_dir.is_dir() {
            if let Some((key, file)) = used.iter().next() {
                problems.push(format!(
                    "{name}：{} 取了文案「{key}」，但这个 crate 没有 locales/ui.yml",
                    file.display()
                ));
            }
            continue;
        }
        let catalog = rust_i18n_support::load_locales(&locales_dir.to_string_lossy(), |_| false);
        let keys: BTreeSet<&String> = catalog.values().flat_map(BTreeMap::keys).collect();
        for key in &keys {
            for (locale, _) in LOCALES {
                if !catalog
                    .get(*locale)
                    .is_some_and(|texts| texts.contains_key(*key))
                {
                    problems.push(format!("{name}：文案「{key}」缺 {locale}"));
                }
            }
        }
        for (key, file) in &used {
            if !keys.contains(key) {
                problems.push(format!(
                    "{name}：{} 取的文案「{key}」不在 locales 里",
                    file.display()
                ));
            }
        }
        for key in keys {
            if !used.contains_key(key) {
                problems.push(format!("{name}：文案「{key}」没有代码在取，删掉"));
            }
        }
    }
    assert!(problems.is_empty(), "\n{}", problems.join("\n"));
}

#[test]
fn key_scan_skips_other_macros() {
    let source = r#"
        format!("not a key");
        t!("a.b");
        text!( "c" , name = x);
        rust_i18n::t!("d");
        let s = t!(dynamic);
    "#;
    let keys: Vec<String> = used_keys(source).into_iter().collect();
    assert_eq!(keys, ["a.b", "c", "d"]);
}
