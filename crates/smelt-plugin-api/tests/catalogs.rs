//! 各 crate 的界面文案（`locales/ui.yml`，见 `docs/i18n.md`）：
//!
//! 1. 每条文案 App 带的每种语言都有；
//! 2. 代码里 `t!("…")`、`text!("…")` 取的键都在本 crate 的文案里；
//! 3. 文案里的键都有代码在取，不留死条目；
//! 4. 用了 `t!` / `text!` 的 crate 都带着文案；
//! 5. 文案里的插值 `%{name}` 取它的代码都传了，没有漏写 `%` 的 `{name}`。各语言可以用不同的插值
//!    （语序、说法不同），代码按需都传。
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

/// 源码里 `t!("key", …)` 和 `text!("key", …)` 的键，和这些调用传的插值名（所有调用处合起来）。
/// 前面紧挨着字母、数字、下划线的不算（`format!(` 也以 `t!(` 结尾）。
fn used_keys(source: &str) -> BTreeMap<String, BTreeSet<String>> {
    let mut keys: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
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
                keys.entry(after[..end].to_string())
                    .or_default()
                    .extend(argument_names(&after[end + 1..]));
            }
        }
    }
    keys
}

/// 宏调用键后面的参数名：`, name = value`、`, name => value`，或简写的 `, name`。读到配平的 `)` 为止。
fn argument_names(args: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut depth = 0usize;
    let mut segment = String::new();
    let mut in_string = false;
    let mut escaped = false;
    for c in args.chars() {
        if in_string {
            segment.push(c);
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                segment.push(c);
            }
            '(' | '[' | '{' => {
                depth += 1;
                segment.push(c);
            }
            ')' | ']' | '}' if depth == 0 => break,
            ')' | ']' | '}' => {
                depth -= 1;
                segment.push(c);
            }
            ',' if depth == 0 => segments.push(std::mem::take(&mut segment)),
            _ => segment.push(c),
        }
    }
    segments.push(segment);
    segments
        .iter()
        .filter_map(|segment| {
            let segment = segment.trim();
            let name = segment
                .split_once("=>")
                .or_else(|| segment.split_once('='))
                .map_or(segment, |(name, _)| name)
                .trim();
            (!name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_'))
                .then(|| name.to_string())
        })
        .collect()
}

/// 文案里的插值名（`%{name}`）。
fn placeholders(text: &str) -> BTreeSet<&str> {
    text.split("%{")
        .skip(1)
        .filter_map(|rest| rest.split_once('}').map(|(name, _)| name))
        .collect()
}

/// 漏写 `%` 的插值：`{name}` 前面不是 `%`，也不是 `{{name}}` 这种要原样显示的双括号。
fn bare_placeholders(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    for (at, _) in text.match_indices('{') {
        let before = at.checked_sub(1).map(|ix| bytes[ix]);
        if matches!(before, Some(b'%' | b'{')) || bytes.get(at + 1) == Some(&b'{') {
            continue;
        }
        let Some((name, _)) = text[at + 1..].split_once('}') else {
            continue;
        };
        if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            found.push(name);
        }
    }
    found
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
        let mut passed: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for file in &sources {
            // 注释里举例的不算（文档里会写 `text!("…")` 的用法）。
            let source: String = std::fs::read_to_string(file)
                .unwrap_or_default()
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .flat_map(|line| [line, "\n"])
                .collect();
            for (key, names) in used_keys(&source) {
                used.entry(key.clone()).or_insert_with(|| file.clone());
                passed.entry(key).or_default().extend(names);
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
                let Some(text) = catalog.get(*locale).and_then(|texts| texts.get(*key)) else {
                    problems.push(format!("{name}：文案「{key}」缺 {locale}"));
                    continue;
                };
                for bare in bare_placeholders(text) {
                    problems.push(format!(
                        "{name}：文案「{key}」的 {locale} 写了 {{{bare}}}，插值要写 %{{{bare}}}"
                    ));
                }
                let Some(names) = passed.get(*key) else {
                    continue;
                };
                for placeholder in placeholders(text) {
                    if !names.contains(placeholder) {
                        problems.push(format!(
                            "{name}：文案「{key}」的 {locale} 用了 %{{{placeholder}}}，取它的代码没传"
                        ));
                    }
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
fn argument_scan_reads_names_not_values() {
    assert_eq!(
        argument_names(r#", count = items.len(), name => f(a, "x)"), short)"#),
        ["count", "name", "short"]
    );
    assert!(argument_names(")").is_empty());
}

#[test]
fn placeholder_scan_tells_bare_braces_from_literal_ones() {
    assert_eq!(
        placeholders("%{count} of %{total}, {{payload}}"),
        BTreeSet::from(["count", "total"])
    );
    assert_eq!(bare_placeholders("failed: {error}"), ["error"]);
    assert!(bare_placeholders("use {{payload}} or %{name}").is_empty());
    assert!(bare_placeholders("JSON like {\"a\": 1}").is_empty());
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
    let keys = used_keys(source);
    assert_eq!(keys.keys().collect::<Vec<_>>(), ["a.b", "c", "d"]);
    assert_eq!(keys["c"], BTreeSet::from(["name".to_string()]));
}
